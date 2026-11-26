//! BD Marketplace API — thin Axum handlers over individually callable
//! service functions.
//!
//! Wave 1: stateless domain endpoints (delivery charge, cart split,
//! commission). Wave 2: Postgres persistence (SQLx), phone + OTP + JWT
//! auth, stores and products. Wave 3: carts, split orders and COD
//! checkout (`cart` and `orders` modules). Wave 4: weight-based
//! delivery, buyer order cancellation with restock, and receipts.
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
pub mod products;
pub mod stores;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use domain::{
    calculate_delivery_charge, compute_commission, split_cart_by_seller, CartItem,
    CommissionQuote, DeliveryZone, SellerSubOrder,
};
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
        }
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
        // Wave 3 — seller sub-orders.
        .route(
            "/v1/seller/sub-orders",
            get(orders::list_seller_sub_orders_handler),
        )
        .route(
            "/v1/seller/sub-orders/:id/status",
            patch(orders::update_sub_order_status_handler),
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
