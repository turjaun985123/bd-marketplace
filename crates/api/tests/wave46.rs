//! Wave 46 integration test: a bounded, deterministic LOAD SMOKE
//! over the hot anonymous public read paths — the product list,
//! product detail, search and the public storefront read.
//!
//! This is a regression smoke, NOT a benchmark. It fires a fixed
//! burst (8 concurrent workers x 25 requests = 200 requests,
//! round-robin across the 4 routes, so exactly 50 per route)
//! through the in-process router and asserts only three things:
//! every response is 200, every route really received its 50
//! requests (no silently skipped workers), and the whole burst
//! finishes under a generous 120-second pathology ceiling. The
//! ceiling exists to catch accidental N+1 storms or
//! missing-index table scans at seed scale — it is NOT a
//! performance budget, and per-route latencies are printed for
//! the CI log as observational data only, never asserted.
//!
//! Helpers are copied from the Wave 9 test on purpose, since
//! integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so
//! a plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL` for
//! the rust job.

use std::time::{Duration, Instant};

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

/// Burst shape: fixed counts, fixed concurrency — deterministic
/// by construction, so a failure is a regression, not weather.
const WORKERS: usize = 8;
const REQUESTS_PER_WORKER: usize = 25;
const TOTAL_REQUESTS: usize = WORKERS * REQUESTS_PER_WORKER;
const PER_ROUTE: usize = TOTAL_REQUESTS / 4;

/// Route labels in round-robin order; index = `global % 4`.
const ROUTES: [&str; 4] = ["products_list", "product_detail", "search", "storefront"];

const STORE_SLUG: &str = "wave46-store";
const SEARCH_TERM: &str = "wave46";

/// Send one JSON request through the router and return (status, body).
async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let request = match body {
        Some(value) => builder
            .header("Content-Type", "application/json")
            .body(Body::from(value.to_string()))
            .expect("request builds"),
        None => builder.body(Body::empty()).expect("request builds"),
    };
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("router responds");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response is JSON")
    };
    (status, parsed)
}

/// One anonymous GET against the router, timed end to end
/// (dispatch + full body read). The body is drained but not
/// parsed — the smoke asserts statuses, not payloads.
async fn timed_get(app: &Router, uri: &str) -> (StatusCode, Duration) {
    let started = Instant::now();
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .expect("request builds");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("router responds");
    let status = response.status();
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    (status, started.elapsed())
}

/// Full dev-mode OTP login for a phone number; returns the JWT.
async fn login(app: &Router, phone: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/auth/request-otp",
        Some(json!({ "phone": phone })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "request-otp {phone}: {body}");
    let dev_otp = body["dev_otp"]
        .as_str()
        .expect("dev mode returns dev_otp")
        .to_string();
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/auth/verify-otp",
        Some(json!({ "phone": phone, "code": dev_otp })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "verify-otp {phone}: {body}");
    body["token"].as_str().expect("token issued").to_string()
}

/// Create a store for the token's user; returns the store id.
async fn create_store(app: &Router, token: &str, slug: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body["id"].as_str().expect("store id").to_string()
}

/// Create an active, categorised product; returns the product id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    category: &str,
    price_poisha: i64,
) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "category": category,
            "price_poisha": price_poisha,
            "stock": 5,
            "status": "active"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

#[tokio::test]
async fn wave46_public_read_load_smoke() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 46 load smoke");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         product_variants, products, store_members, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode on (OTP login for the seeding seller).
    let state = AppState::new(Some(pool.clone()), "wave46-test-secret".to_string(), true);
    let app = build_app(state);

    // Seed: one seller, one store, twelve active products across
    // three categories, every title carrying the search term so
    // list, detail, search and storefront reads all have real rows.
    let seller = login(&app, "01746464646").await;
    let store = create_store(&app, &seller, STORE_SLUG).await;
    let categories = ["clothing", "electronics", "grocery"];
    let mut product_ids: Vec<String> = Vec::new();
    for n in 1..=12 {
        let id = create_product(
            &app,
            &seller,
            &store,
            &format!("Wave46 Smoke Product {n:02}"),
            categories[(n - 1) % categories.len()],
            50_000 + (n as i64) * 1_000,
        )
        .await;
        product_ids.push(id);
    }

    // Build the full request plan BEFORE spawning: 200 entries in
    // strict round-robin (route = global index % 4), with product
    // detail rotating over the seeded ids. Each route therefore
    // receives exactly PER_ROUTE requests by construction.
    let mut plan: Vec<(usize, String)> = Vec::with_capacity(TOTAL_REQUESTS);
    let mut detail_cursor = 0usize;
    for i in 0..TOTAL_REQUESTS {
        let route = i % ROUTES.len();
        let uri = match route {
            0 => "/v1/products".to_string(),
            1 => {
                let id = &product_ids[detail_cursor % product_ids.len()];
                detail_cursor += 1;
                format!("/v1/products/{id}")
            }
            2 => format!("/v1/search?q={SEARCH_TERM}"),
            _ => format!("/v1/stores/{STORE_SLUG}"),
        };
        plan.push((route, uri));
    }

    // Fire the burst: WORKERS tokio tasks, each owning a Router
    // clone (the same `app.clone().oneshot(...)` mechanism the
    // `call` helper uses per request) and a strided slice of the
    // plan (every WORKERS-th entry, in order), run sequentially
    // within the worker. Results come back through the join
    // handles — no shared mutable state anywhere.
    let burst_started = Instant::now();
    let mut handles = Vec::with_capacity(WORKERS);
    for worker in 0..WORKERS {
        let slice: Vec<(usize, String)> = plan
            .iter()
            .enumerate()
            .filter(|(idx, _)| *idx % WORKERS == worker)
            .map(|(_, entry)| entry.clone())
            .collect();
        assert_eq!(slice.len(), REQUESTS_PER_WORKER, "worker slice size");
        let worker_app = app.clone();
        handles.push(tokio::spawn(async move {
            let mut results: Vec<(usize, StatusCode, Duration)> =
                Vec::with_capacity(slice.len());
            for (route, uri) in slice {
                let (status, elapsed) = timed_get(&worker_app, &uri).await;
                results.push((route, status, elapsed));
            }
            results
        }));
    }
    let mut all: Vec<(usize, StatusCode, Duration)> = Vec::with_capacity(TOTAL_REQUESTS);
    for handle in handles {
        let mut part = handle.await.expect("burst worker finished");
        all.append(&mut part);
    }
    let burst_elapsed = burst_started.elapsed();

    // Assertion (c): the whole plan really ran — 200 results.
    assert_eq!(
        all.len(),
        TOTAL_REQUESTS,
        "burst produced {} results, expected {TOTAL_REQUESTS}",
        all.len()
    );

    // Assertion (a): every single response is 200 — any 4xx/5xx
    // fails, naming the route and the status.
    for (route, status, _) in &all {
        assert_eq!(
            *status,
            StatusCode::OK,
            "burst request to route {} returned {status}",
            ROUTES[*route]
        );
    }

    // Assertion (c, per route) + observational latency summary:
    // exactly PER_ROUTE results per route (proves the round-robin
    // ran and no worker silently skipped), and one printed line
    // per route for the CI log. Latencies are printed, NEVER
    // asserted — a shared CI runner's timing is weather, not a
    // contract.
    for (route, label) in ROUTES.iter().enumerate() {
        let mut latencies: Vec<u128> = all
            .iter()
            .filter(|(r, _, _)| *r == route)
            .map(|(_, _, elapsed)| elapsed.as_millis())
            .collect();
        assert_eq!(
            latencies.len(),
            PER_ROUTE,
            "route {label} received {} requests, expected {PER_ROUTE}",
            latencies.len()
        );
        latencies.sort_unstable();
        println!(
            "wave46 load smoke: route={label} count={} min_ms={} median_ms={} max_ms={}",
            latencies.len(),
            latencies[0],
            latencies[latencies.len() / 2],
            latencies[latencies.len() - 1],
        );
    }
    println!(
        "wave46 load smoke: total_requests={} workers={} burst_wall_ms={}",
        all.len(),
        WORKERS,
        burst_elapsed.as_millis()
    );

    // Assertion (b): the pathology ceiling. 120 seconds for 200
    // in-process reads is deliberately generous — a healthy run
    // is far under it. It exists to catch accidental N+1 storms
    // or missing-index table scans at seed scale; it is NOT a
    // performance budget and must not be tightened into one.
    assert!(
        burst_elapsed < Duration::from_secs(120),
        "burst of {TOTAL_REQUESTS} requests took {burst_elapsed:?} — \
         pathological slowdown (the 120s ceiling is a smoke guard, \
         not a performance budget)"
    );
}
