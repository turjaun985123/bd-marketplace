//! Thin Axum API for the marketplace.
//!
//! Handlers do exactly three things: parse/validate JSON, call ONE domain
//! function, return its result. All business logic lives in `crates/domain`
//! so the worker, CLI tools and future clients reuse the same functions.
//!
//! Wave 1: no database and no Redis are wired in. See `config` TODO below.

use axum::{
    routing::{get, post},
    Json, Router,
};
use domain::{
    calculate_delivery_charge, compute_commission, split_cart_by_seller, CartItem,
    CommissionQuote, DeliveryZone, SellerSubOrder,
};
use serde::{Deserialize, Serialize};

// TODO(wave-2): real configuration + persistence.
// - DATABASE_URL (Postgres via SQLx) — not wired in Wave 1 on purpose.
// - REDIS_URL (OTP/session store, job queue) — not wired in Wave 1.
// Until then the API is stateless and every endpoint is a pure call into
// the domain crate. Do not add fake DB code here.

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    version: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "bd-marketplace-api",
        version: env!("CARGO_PKG_VERSION"),
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

async fn delivery_charge(
    Json(req): Json<DeliveryChargeRequest>,
) -> Json<DeliveryChargeResponse> {
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

async fn commission_quote(
    Json(req): Json<CommissionQuoteRequest>,
) -> Json<CommissionQuote> {
    Json(compute_commission(req.subtotal_poisha, req.rate_bps))
}

fn app() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/delivery-charge", post(delivery_charge))
        .route("/v1/cart/split", post(split_cart))
        .route("/v1/commission/quote", post(commission_quote))
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("API_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8080);
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind API listener");
    println!("bd-marketplace api listening on {addr}");
    axum::serve(listener, app()).await.expect("API server error");
}
