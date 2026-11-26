//! BD Marketplace API — thin Axum handlers over individually callable
//! service functions.
//!
//! Wave 1: stateless domain endpoints (delivery charge, cart split,
//! commission). Wave 2: Postgres persistence (SQLx), phone + OTP + JWT
//! auth, stores and products. Wave 3: carts, split orders and COD
//! checkout (`cart` and `orders` modules). Wave 4: weight-based
//! delivery, buyer order cancellation with restock, and receipts.
//! Wave 5: the online payments foundation (`payments` module) —
//! method availability, dev-simulated gateway checkout, signed
//! webhook callbacks and payment retry.
//! Wave 6: courier shipments (`shipments` module) — the real
//! Steadfast adapter behind merchant credentials (or the DEV courier
//! simulator behind its flag), one shipment per sub-order, tracking
//! refresh that syncs sub-order status, and courier availability.
//! Wave 7: the Pathao and RedX adapters (same credential-gated
//! pattern), per-courier delivery-charge estimates, and smart
//! routing — booking with courier `auto` picks the cheapest
//! available courier via the domain `choose_courier` function.
//! Wave 8: reviews and ratings (`reviews` module) — a buyer may
//! review a product only after a delivered sub-order proves they
//! received it; one review per buyer per product; the store owner
//! may post one reply per review; and the product view gains a live
//! rating summary from the single aggregate join in `products`.
//! Wave 9: seller dashboard, earnings and payout requests
//! (`payouts` module) — a store earns only from DELIVERED
//! sub-orders (goods minus the commission snapshot); one earnings
//! aggregate feeds the dashboard, the balance endpoint and payout
//! creation; sellers save one payout account and request payouts
//! against their available balance (platform minimum 500 taka);
//! approval and payment processing land with the admin wave.
//! Handlers only parse/validate,
//! call ONE service function, and return its result — business rules
//! live in the `domain` crate (pure functions) or in the service
//! functions in the per-resource modules, never inside handlers.
//!
//! Database behaviour: when `DATABASE_URL` is set at startup the API
//! connects and runs the workspace migrations automatically. When it is
//! NOT set the API still starts and serves the Wave 1 stateless
//! endpoints; DB-backed endpoints return a clear 503 instead of panicking.

pub mod auth;
pub mod cart;
pub mod orders;
pub mod payments;
pub mod payouts;
pub mod products;
pub mod reviews;
pub mod shipments;
pub mod stores;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use ::courier::{CourierConfig, CouriersConfig, PathaoConfig, RedxConfig};
use domain::{
    calculate_delivery_charge, compute_commission, split_cart_by_seller, CartItem,
    CommissionQuote, DeliveryZone, SellerSubOrder,
};
use ::payments::{GatewayConfig, PaymentsConfig};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tower_http::cors::CorsLayer;

/// Shared application state. `pool` is `None` when the API was started
/// without `DATABASE_URL` — DB endpoints must check via `require_pool`.
#[derive(Clone)]
pub struct AppState {
    pub pool: Option<PgPool>,
    /// HMAC secret for signing JWTs.
    pub jwt_secret: String,
    /// True only in explicit dev mode (`APP_ENV=dev` or `SMS_DEV_MODE=true`).
    /// Gates the log SMS sender and the `dev_otp` response field.
    pub sms_dev_mode: bool,
    /// Platform commission rate in basis points (800 = 8%). Read once at
    /// startup from `COMMISSION_RATE_BPS` (see `from_env`); checkout
    /// passes it to the domain `build_order_totals` function.
    pub commission_rate_bps: u32,
    /// Payment configuration (Wave 5): the DEV simulator switch and
    /// secret, plus one merchant credential set per gateway. Built
    /// from the environment in `from_env`; tests set it explicitly
    /// via `with_payments_config`.
    pub payments_config: PaymentsConfig,
    /// Courier configuration (Wave 6): the DEV simulator switch,
    /// Steadfast's merchant credentials, and the default courier.
    /// Built from the environment in `from_env`; tests set it
    /// explicitly via `with_couriers_config`.
    pub couriers_config: CouriersConfig,
}

/// Default commission rate: 8% (matches the MVP plan's worked example).
pub const DEFAULT_COMMISSION_RATE_BPS: u32 = 800;

impl AppState {
    pub fn new(pool: Option<PgPool>, jwt_secret: String, sms_dev_mode: bool) -> Self {
        AppState {
            pool,
            jwt_secret,
            sms_dev_mode,
            commission_rate_bps: DEFAULT_COMMISSION_RATE_BPS,
            payments_config: PaymentsConfig::default(),
            couriers_config: CouriersConfig::default(),
        }
    }

    /// Replace the payments configuration (used by `from_env` and by
    /// tests that need the DEV payment simulator on or off).
    pub fn with_payments_config(mut self, payments_config: PaymentsConfig) -> Self {
        self.payments_config = payments_config;
        self
    }

    /// Replace the couriers configuration (used by `from_env` and by
    /// tests that need the DEV courier simulator on or off).
    pub fn with_couriers_config(mut self, couriers_config: CouriersConfig) -> Self {
        self.couriers_config = couriers_config;
        self
    }

    /// Build state from environment variables around an optional pool.
    ///
    /// WARNING: if `JWT_SECRET` is not set we fall back to a well-known
    /// DEV-ONLY secret and log a loud warning. Never run production with
    /// the fallback — anyone can forge tokens signed with a public secret.
    pub fn from_env(pool: Option<PgPool>) -> Self {
        let jwt_secret = match std::env::var("JWT_SECRET") {
            Ok(secret) if !secret.is_empty() => secret,
            _ => {
                eprintln!(
                    "WARNING: JWT_SECRET is not set — using the DEV-ONLY default secret. \
                     Tokens can be forged; set JWT_SECRET before any real deployment."
                );
                "dev-only-insecure-jwt-secret-change-me".to_string()
            }
        };
        let sms_dev_mode = std::env::var("APP_ENV").map(|v| v == "dev").unwrap_or(false)
            || std::env::var("SMS_DEV_MODE").map(|v| v == "true").unwrap_or(false);
        let mut state = AppState::new(pool, jwt_secret, sms_dev_mode);
        // COMMISSION_RATE_BPS is read once, here at startup; invalid or
        // out-of-range values keep the default rather than breaking money
        // maths later.
        if let Ok(raw) = std::env::var("COMMISSION_RATE_BPS") {
            match raw.parse::<u32>() {
                Ok(rate) if rate <= 10_000 => state.commission_rate_bps = rate,
                _ => eprintln!(
                    "WARNING: COMMISSION_RATE_BPS='{raw}' is invalid — \
                     using the default {DEFAULT_COMMISSION_RATE_BPS} bps"
                ),
            }
        }
        state.payments_config = payments_config_from_env();
        state.couriers_config = couriers_config_from_env();
        state
    }

    /// The pool, or a clear 503 when the API runs without a database.
    pub fn require_pool(&self) -> Result<&PgPool, ApiError> {
        self.pool
            .as_ref()
            .ok_or_else(|| ApiError::service_unavailable("database not configured"))
    }
}

/// One error type for the whole API: an HTTP status + a message, rendered
/// as `{"error": "..."}`. Service functions return this; handlers never
/// build ad-hoc error responses.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        ApiError {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::UNAUTHORIZED, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::FORBIDDEN, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::CONFLICT, message)
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::SERVICE_UNAVAILABLE, message)
    }

    /// Map a SQLx error to a 500 without leaking internals to clients.
    pub fn internal(context: &str, err: sqlx::Error) -> Self {
        eprintln!("database error in {context}: {err}");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("database error ({context})"),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({ "error": self.message }));
        (self.status, body).into_response()
    }
}

/// A non-empty environment variable, or `None`. Credential values
/// are only ever held in memory inside `PaymentsConfig` — never
/// logged, never returned by any endpoint.
fn env_value(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

/// Build the Wave 5 payments configuration from the environment:
/// `PAYMENTS_DEV_MODE` gates the DEV simulator (with a loud startup
/// warning — it must never be on in production), `PAYMENTS_DEV_SECRET`
/// is its HMAC secret (defaulting to a public dev value, with a
/// warning, only when dev mode is on), and each gateway's merchant
/// credentials come from its `*_APP_KEY` / `*_MERCHANT_ID` style
/// variables (see `.env.example` for the full list per gateway).
fn payments_config_from_env() -> PaymentsConfig {
    let dev_mode = std::env::var("PAYMENTS_DEV_MODE")
        .map(|v| v == "true")
        .unwrap_or(false);
    let dev_secret = match env_value("PAYMENTS_DEV_SECRET") {
        Some(secret) => secret,
        None if dev_mode => {
            eprintln!(
                "WARNING: PAYMENTS_DEV_SECRET is not set — using the public \
                 default 'dev-secret' for DEV callback signatures."
            );
            "dev-secret".to_string()
        }
        None => String::new(),
    };
    if dev_mode {
        eprintln!(
            "WARNING: PAYMENTS_DEV_MODE is ON — the DEV payment simulator is \
             enabled. Simulated payments are not real money. NEVER enable \
             this in production."
        );
    }
    PaymentsConfig {
        dev_mode,
        dev_secret,
        bkash: GatewayConfig {
            merchant_id: None,
            api_key: env_value("BKASH_APP_KEY"),
            secret: env_value("BKASH_APP_SECRET"),
        },
        nagad: GatewayConfig {
            merchant_id: env_value("NAGAD_MERCHANT_ID"),
            api_key: env_value("NAGAD_API_KEY"),
            secret: None,
        },
        sslcommerz: GatewayConfig {
            merchant_id: env_value("SSLCOMMERZ_STORE_ID"),
            api_key: None,
            secret: env_value("SSLCOMMERZ_STORE_PASSWORD"),
        },
        shurjopay: GatewayConfig {
            merchant_id: env_value("SHURJOPAY_USERNAME"),
            api_key: None,
            secret: env_value("SHURJOPAY_PASSWORD"),
        },
    }
}

/// Build the couriers configuration from the environment:
/// `COURIER_DEV_MODE` gates the DEV courier simulator (with a loud
/// startup warning — it must never be on in production),
/// `STEADFAST_API_KEY` / `STEADFAST_SECRET_KEY` are the platform's
/// Steadfast merchant credentials, `STEADFAST_BASE_URL` overrides
/// the production API base URL (sandbox/testing), and
/// `DEFAULT_COURIER` names the courier sellers get by default.
/// Wave 7 adds Pathao (`PATHAO_CLIENT_ID` / `PATHAO_CLIENT_SECRET`
/// / `PATHAO_STORE_ID`, `PATHAO_BASE_URL` overrides the Hermes API
/// base URL) and RedX (`REDX_API_KEY`, `REDX_BASE_URL` overrides
/// the open-API base URL).
fn couriers_config_from_env() -> CouriersConfig {
    let dev_mode = std::env::var("COURIER_DEV_MODE")
        .map(|v| v == "true")
        .unwrap_or(false);
    if dev_mode {
        eprintln!(
            "WARNING: COURIER_DEV_MODE is ON — the DEV courier simulator is \
             enabled. Simulated shipments are not real bookings. NEVER \
             enable this in production."
        );
    }
    CouriersConfig {
        dev_mode,
        steadfast: CourierConfig {
            api_key: env_value("STEADFAST_API_KEY"),
            secret_key: env_value("STEADFAST_SECRET_KEY"),
            base_url: env_value("STEADFAST_BASE_URL")
                .unwrap_or_else(|| ::courier::DEFAULT_STEADFAST_BASE_URL.to_string()),
        },
        pathao: PathaoConfig {
            client_id: env_value("PATHAO_CLIENT_ID"),
            client_secret: env_value("PATHAO_CLIENT_SECRET"),
            store_id: env_value("PATHAO_STORE_ID"),
            base_url: env_value("PATHAO_BASE_URL")
                .unwrap_or_else(|| ::courier::DEFAULT_PATHAO_BASE_URL.to_string()),
        },
        redx: RedxConfig {
            api_key: env_value("REDX_API_KEY"),
            base_url: env_value("REDX_BASE_URL")
                .unwrap_or_else(|| ::courier::DEFAULT_REDX_BASE_URL.to_string()),
        },
        default_courier: env_value("DEFAULT_COURIER")
            .unwrap_or_else(|| "steadfast".to_string()),
    }
}

/// Run the embedded workspace migrations (SQLx migrate format, in
/// `migrations/` at the workspace root) against a pool.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("../../migrations").run(pool).await
}

// ---------------------------------------------------------------------------
// Wave 1 stateless endpoints (unchanged behaviour; no database involved).
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    version: &'static str,
    database: &'static str,
}

async fn health(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "bd-marketplace-api",
        version: env!("CARGO_PKG_VERSION"),
        database: if state.pool.is_some() {
            "configured"
        } else {
            "not configured"
        },
    })
}

#[derive(Deserialize)]
struct DeliveryChargeRequest {
    zone: DeliveryZone,
    weight_kg: u32,
    #[serde(default)]
    cod_amount_poisha: i64,
}

#[derive(Serialize)]
struct DeliveryChargeResponse {
    delivery_charge_poisha: i64,
}

async fn delivery_charge(Json(req): Json<DeliveryChargeRequest>) -> Json<DeliveryChargeResponse> {
    let charge = calculate_delivery_charge(req.zone, req.weight_kg, req.cod_amount_poisha);
    Json(DeliveryChargeResponse {
        delivery_charge_poisha: charge,
    })
}

#[derive(Deserialize)]
struct SplitCartRequest {
    items: Vec<CartItem>,
}

#[derive(Serialize)]
struct SplitCartResponse {
    sub_orders: Vec<SellerSubOrder>,
}

async fn split_cart(Json(req): Json<SplitCartRequest>) -> Json<SplitCartResponse> {
    Json(SplitCartResponse {
        sub_orders: split_cart_by_seller(&req.items),
    })
}

#[derive(Deserialize)]
struct CommissionQuoteRequest {
    subtotal_poisha: i64,
    rate_bps: u32,
}

async fn commission_quote(Json(req): Json<CommissionQuoteRequest>) -> Json<CommissionQuote> {
    Json(compute_commission(req.subtotal_poisha, req.rate_bps))
}

/// Build the full router around a state. Used by `main` and by the
/// integration tests (which drive it via tower oneshot, no TCP port).
pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        // Wave 1 — stateless domain calls.
        .route("/v1/delivery-charge", post(delivery_charge))
        .route("/v1/cart/split", post(split_cart))
        .route("/v1/commission/quote", post(commission_quote))
        // Wave 2 — auth (phone + OTP + JWT).
        .route("/v1/auth/request-otp", post(auth::request_otp))
        .route("/v1/auth/verify-otp", post(auth::verify_otp))
        // Wave 2 — stores.
        .route("/v1/stores", post(stores::create_store))
        .route("/v1/stores/:slug", get(stores::get_store))
        .route(
            "/v1/stores/:slug/products",
            get(products::list_store_products),
        )
        // Wave 2 — products.
        .route(
            "/v1/products",
            post(products::create_product).get(products::list_products_handler),
        )
        .route(
            "/v1/products/:id",
            get(products::get_product).patch(products::update_product),
        )
        // Wave 3 — cart (one cart per signed-in buyer).
        .route("/v1/cart", get(cart::get_cart).delete(cart::clear))
        .route("/v1/cart/items", post(cart::add_item))
        .route(
            "/v1/cart/items/:product_id",
            patch(cart::set_item_quantity).delete(cart::remove_item),
        )
        // Wave 3 — checkout + buyer orders.
        .route(
            "/v1/orders",
            post(orders::create_order).get(orders::list_orders),
        )
        .route("/v1/orders/:id", get(orders::get_order))
        // Wave 4 — buyer cancel (with restock) + receipt view.
        .route("/v1/orders/:id/cancel", post(orders::cancel_order))
        .route("/v1/orders/:id/receipt", get(orders::get_receipt))
        // Wave 5 — payments: availability, gateway webhook, retry,
        // and the dev simulator helper (404s when dev mode is off).
        .route("/v1/payments/methods", get(payments::list_methods))
        .route("/v1/payments/webhook/:method", post(payments::webhook))
        .route("/v1/payments/:id/retry", post(payments::retry))
        .route(
            "/v1/payments/:id/dev-simulate-success",
            post(payments::dev_simulate),
        )
        // Wave 3 — seller sub-orders.
        .route(
            "/v1/seller/sub-orders",
            get(orders::list_seller_sub_orders_handler),
        )
        .route(
            "/v1/seller/sub-orders/:id/status",
            patch(orders::update_sub_order_status_handler),
        )
        // Wave 6 — couriers + shipments: availability, booking a
        // shipment for a confirmed sub-order, tracking refresh.
        // Wave 7 — per-courier estimates + smart routing.
        .route("/v1/couriers", get(shipments::list_couriers))
        .route("/v1/couriers/estimate", get(shipments::courier_estimates))
        .route(
            "/v1/seller/sub-orders/:id/shipment",
            post(shipments::create_shipment),
        )
        .route("/v1/shipments/:id/refresh", post(shipments::refresh))
        // Wave 8 — reviews: buyers review products they received,
        // edit/delete their own; store owners reply once per review.
        .route(
            "/v1/products/:id/reviews",
            post(reviews::create_review_handler).get(reviews::list_reviews_handler),
        )
        .route(
            "/v1/reviews/:id",
            patch(reviews::update_review_handler).delete(reviews::delete_review_handler),
        )
        .route(
            "/v1/seller/reviews/:id/reply",
            post(reviews::create_reply_handler).patch(reviews::update_reply_handler),
        )
        // Wave 9 — seller dashboard, balance, payout account and
        // payout requests (approval/payment land with the admin wave).
        .route("/v1/seller/dashboard", get(payouts::dashboard_handler))
        .route("/v1/seller/balance", get(payouts::balance_handler))
        .route(
            "/v1/seller/payout-account",
            get(payouts::get_payout_account_handler)
                .put(payouts::save_payout_account_handler),
        )
        .route(
            "/v1/seller/payouts",
            post(payouts::request_payout_handler).get(payouts::list_payouts_handler),
        )
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Connect to Postgres (when configured), migrate, and serve. Called by
/// the binary's `main`; kept in the library so behaviour stays testable.
pub async fn run() {
    let pool = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => {
            let pool = PgPool::connect(&url)
                .await
                .expect("failed to connect to Postgres via DATABASE_URL");
            run_migrations(&pool)
                .await
                .expect("failed to run database migrations");
            println!("database connected, migrations applied");
            Some(pool)
        }
        _ => {
            println!(
                "DATABASE_URL not set — starting without a database; \
                 DB endpoints will return 503"
            );
            None
        }
    };

    let state = AppState::from_env(pool);
    let port: u16 = std::env::var("API_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8080);
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind API listener");
    println!("bd-marketplace api listening on {addr}");
    axum::serve(listener, build_app(state))
        .await
        .expect("API server error");
}
