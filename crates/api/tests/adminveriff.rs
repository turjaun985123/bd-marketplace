//! Admin verifications remake, part F: the queue envelope,
//! search discipline, and the export split. Pins: the list
//! is an envelope `{items, total, page, limit, summary}` with
//! whole-queue summary counts that do NOT inherit the status
//! filter; pagination is stable and disjoint across pages;
//! search matches store slug but NEVER an NID fragment (no
//! probing oracle around the audited full view); the
//! unscoped owner export carries a `verification` section
//! (masked views, one per store) that contains no full NID,
//! while a manager's scoped (`view_finances`) export contains
//! no `verification` key at all.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

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

/// Submit a verification application as the store's seller.
async fn submit(app: &Router, token: &str, nid: &str) -> (StatusCode, Value) {
    call(
        app,
        Method::PUT,
        "/v1/seller/store/verification",
        Some(json!({ "nid_number": nid })),
        Some(token),
    )
    .await
}

#[tokio::test]
async fn adminverif_envelope_search_and_export_split() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin verifications (envelope) test");
            return;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, store_verification_events, \
         store_verifications, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         products, api_keys, store_members, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let state = AppState::new(Some(pool.clone()), "adminveriff-test-secret".to_string(), true);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // Two pending applications and one rejected.
    let seller_a = login(&app, "01722222222").await;
    let store_a = create_store(&app, &seller_a, "adminveriff-alpha").await;
    let nid_a = "1234567890123";
    let (status, view) = submit(&app, &seller_a, nid_a).await;
    assert_eq!(status, StatusCode::OK, "submit A: {view}");
    let seller_b = login(&app, "01733333333").await;
    let store_b = create_store(&app, &seller_b, "adminveriff-beta").await;
    let (status, view) = submit(&app, &seller_b, "1234567890").await;
    assert_eq!(status, StatusCode::OK, "submit B: {view}");
    let seller_c = login(&app, "01744444444").await;
    let store_c = create_store(&app, &seller_c, "adminveriff-gamma").await;
    let (status, view) = submit(&app, &seller_c, "12345678901234567").await;
    assert_eq!(status, StatusCode::OK, "submit C: {view}");
    let (status, view) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/verifications/{store_c}/reject"),
        Some(json!({ "note": "ডকুমেন্ট নেই", "expected_version": 1 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reject C: {view}");
    assert_eq!(view["status"], "rejected");

    // The default queue is the pending envelope; the summary
    // counts the WHOLE queue (the rejected row is counted
    // even though the filter hides it).
    let (status, queue) = call(&app, Method::GET, "/v1/admin/verifications", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "queue: {queue}");
    assert_eq!(queue["total"], 2);
    assert_eq!(queue["page"], 1);
    assert_eq!(queue["limit"], 20);
    assert_eq!(queue["summary"]["pending"], 2);
    assert_eq!(queue["summary"]["approved"], 0);
    assert_eq!(queue["summary"]["rejected"], 1);
    let items = queue["items"].as_array().expect("items list");
    assert_eq!(items.len(), 2);
    let queued_ids: Vec<&str> = items
        .iter()
        .filter_map(|row| row["store_id"].as_str())
        .collect();
    assert!(queued_ids.contains(&store_a.as_str()), "A queued: {queued_ids:?}");
    assert!(queued_ids.contains(&store_b.as_str()), "B queued: {queued_ids:?}");

    // Pagination over `all` is stable and disjoint.
    let (status, page_one) = call(
        &app,
        Method::GET,
        "/v1/admin/verifications?status=all&limit=2&page=1",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "page 1: {page_one}");
    assert_eq!(page_one["total"], 3);
    let (status, page_two) = call(
        &app,
        Method::GET,
        "/v1/admin/verifications?status=all&limit=2&page=2",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "page 2: {page_two}");
    let first_ids: Vec<&str> = page_one["items"]
        .as_array()
        .expect("page 1 items")
        .iter()
        .filter_map(|row| row["store_id"].as_str())
        .collect();
    let second_ids: Vec<&str> = page_two["items"]
        .as_array()
        .expect("page 2 items")
        .iter()
        .filter_map(|row| row["store_id"].as_str())
        .collect();
    assert_eq!(first_ids.len(), 2);
    assert_eq!(second_ids.len(), 1);
    assert!(
        first_ids.iter().all(|id| !second_ids.contains(id)),
        "pages disjoint: {first_ids:?} vs {second_ids:?}"
    );

    // Search finds a store by slug — and can NEVER probe an
    // NID fragment: the number is not a search surface.
    let (status, found) = call(
        &app,
        Method::GET,
        "/v1/admin/verifications?status=all&q=adminveriff-beta",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "search slug: {found}");
    assert_eq!(found["total"], 1);
    assert_eq!(found["items"][0]["store_id"], store_b);
    let (status, probed) = call(
        &app,
        Method::GET,
        "/v1/admin/verifications?status=all&q=1234567890",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "search NID fragment: {probed}");
    assert_eq!(probed["total"], 0);

    // The owner export carries the masked verification
    // section; the full NID is nowhere in the document.
    let (status, export) = call(&app, Method::GET, "/v1/seller/export", None, Some(&seller_a)).await;
    assert_eq!(status, StatusCode::OK, "owner export: {export}");
    let section = export["verification"].as_array().expect("verification section");
    assert_eq!(section.len(), 1);
    assert_eq!(section[0]["store_id"], store_a);
    assert_eq!(section[0]["verification"]["nid_masked"], "*********0123");
    assert!(
        !export.to_string().contains(nid_a),
        "export must not contain the full NID: {export}"
    );

    // A manager's scoped export (view_finances) has NO
    // verification key at all — the section is owner-only.
    let manager = login(&app, "01755555555").await;
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/stores/{store_a}/members"),
        Some(json!({ "phone": "01755555555", "role": "manager" })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "seat manager: {body}");
    let (status, scoped) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/export?store_id={store_a}"),
        None,
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "scoped export: {scoped}");
    assert!(
        scoped.get("verification").is_none(),
        "scoped export must not carry the KYC section: {scoped}"
    );
    assert!(
        !scoped.to_string().contains(nid_a),
        "scoped export must not contain the NID: {scoped}"
    );
}
