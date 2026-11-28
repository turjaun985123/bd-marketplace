//! Wave 21 integration test: seller KYC & verification tiers —
//! a seller applies with an NID (the seller view and the admin
//! queue only ever show it MASKED; the full number appears in
//! no JSON body anywhere in this flow), an admin cannot grant
//! the `verified` tier without a trade license on the
//! application, basic and verified approvals land on the
//! store's public `verified_tier`, a rejection carries a note
//! the seller sees and can resubmit after, deciding a
//! non-pending application is a 409, and the KYC payout gate
//! (off by default) blocks unverified stores from payout
//! requests with a verification-specific message while a
//! verified store with no balance still gets the ordinary
//! balance error. The helpers are copied from the Wave 9 test
//! on purpose, since integration test files stay
//! self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

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
async fn submit_verification(
    app: &Router,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    call(
        app,
        Method::PUT,
        "/v1/seller/store/verification",
        Some(body),
        Some(token),
    )
    .await
}

/// Save a bKash payout account for the token's seller.
async fn save_payout_account(app: &Router, token: &str, phone: &str) {
    let (status, body) = call(
        app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "Wave21 Seller",
            "account_number": phone
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save payout account: {body}");
}

/// Request a payout as the token's seller.
async fn request_payout(app: &Router, token: &str) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": 100_000 })),
        Some(token),
    )
    .await
}

/// The error message of a response body, for message assertions.
fn error_text(body: &Value) -> String {
    body["error"].as_str().expect("error message").to_string()
}

#[tokio::test]
async fn wave21_seller_kyc_verification_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 21 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE store_verifications, platform_settings, payouts, \
         payout_accounts, review_replies, reviews, shipment_events, \
         shipments, payment_events, payments, order_items, sub_orders, \
         orders, cart_items, carts, products, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode only: no orders are placed in this wave's flow.
    let state = AppState::new(Some(pool.clone()), "wave21-test-secret".to_string(), true);
    let app = build_app(state);

    // The platform admin (promoted by SQL — the only way in).
    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // Seller A: NID only — headed for the basic tier.
    let seller_a = login(&app, "01722222222").await;
    let store_a = create_store(&app, &seller_a, "wave21-alpha").await;

    // 1. Before applying: status none, tier unverified.
    let (status, view) = call(
        &app,
        Method::GET,
        "/v1/seller/store/verification",
        None,
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "initial view: {view}");
    assert_eq!(view["status"], "none");
    assert_eq!(view["tier"], "unverified");
    assert!(view["nid_masked"].is_null());

    // 2. The admin queue is admin-only.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/verifications",
        None,
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "non-admin queue: {body}");

    // 3. A 9-digit NID is not a BD NID: 400, and nothing is stored.
    let (status, body) = submit_verification(
        &app,
        &seller_a,
        json!({ "nid_number": "123456789" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "9-digit NID: {body}");

    // 4. A valid 13-digit NID applies: pending, and the response
    // carries ONLY the masked form — the full number appears
    // nowhere in the JSON.
    let nid_a = "1234567890123";
    let (status, view) = submit_verification(
        &app,
        &seller_a,
        json!({ "nid_number": nid_a }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "submit A: {view}");
    assert_eq!(view["status"], "pending");
    assert_eq!(view["tier"], "unverified");
    assert_eq!(view["nid_masked"], "*********0123");
    assert!(
        !view.to_string().contains(nid_a),
        "seller view must not contain the full NID: {view}"
    );

    // The seller's own GET agrees, still masked.
    let (status, view) = call(
        &app,
        Method::GET,
        "/v1/seller/store/verification",
        None,
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller view: {view}");
    assert_eq!(view["status"], "pending");
    assert_eq!(view["nid_masked"], "*********0123");
    assert!(!view.to_string().contains(nid_a));

    // 5. The admin queue shows A masked, phoneless-in-full, and
    // without a trade license; the full NID is absent here too.
    let (status, queue) = call(
        &app,
        Method::GET,
        "/v1/admin/verifications?status=pending",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin queue: {queue}");
    let row_a = queue["items"]
        .as_array()
        .expect("queue envelope carries items")
        .iter()
        .find(|row| row["store_id"] == store_a)
        .expect("A is in the pending queue")
        .clone();
    assert_eq!(row_a["store_slug"], "wave21-alpha");
    assert_eq!(row_a["owner_phone_masked"], "017****222");
    assert_eq!(row_a["nid_masked"], "*********0123");
    assert_eq!(row_a["has_trade_license"], false);
    assert!(
        !queue.to_string().contains(nid_a),
        "admin queue must not contain the full NID: {queue}"
    );

    // 6. The full view is the ONE place the number comes back —
    // and only for the admin. Since the admin verifications
    // remake it is an audited POST: the admin states a reason,
    // the read writes an audit row, then the number is served.
    let uri = format!("/v1/admin/verifications/{store_a}/full");
    let (status, full) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "reason": "queue review of wave21-alpha" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin full view: {full}");
    assert_eq!(full["nid_number"], nid_a);
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "reason": "seller trying to self-serve" })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller full view: {body}");

    // 7. `verified` without a trade license: 400.
    let uri = format!("/v1/admin/verifications/{store_a}/approve");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "tier": "verified", "expected_version": 1 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "verified w/o license: {body}");
    assert!(error_text(&body).contains("trade license"), "license msg: {body}");

    // 8. Approve basic: the application is approved, the store
    // tier is basic — and the PUBLIC store view carries it.
    // A submitted no document image, so the approval uses the
    // remake's explicit no-document override (audited).
    let (status, view) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({
            "tier": "basic",
            "expected_version": 1,
            "no_document_override": true
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve basic: {view}");
    assert_eq!(view["status"], "approved");
    assert_eq!(view["tier"], "basic");

    let (status, store) = call(&app, Method::GET, "/v1/stores/wave21-alpha", None, None).await;
    assert_eq!(status, StatusCode::OK, "public store: {store}");
    assert_eq!(store["verified_tier"], "basic");

    // 9. Approving an already-decided application is a 409.
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "tier": "basic", "expected_version": 2 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "approve non-pending: {body}");

    // 10. Seller B: NID + trade license earns the verified tier.
    let seller_b = login(&app, "01733333333").await;
    let store_b = create_store(&app, &seller_b, "wave21-beta").await;
    let (status, view) = submit_verification(
        &app,
        &seller_b,
        json!({
            "nid_number": "1234567890",
            "trade_license_number": "TL-2024-9988",
            "document_url": "https://example.com/beta-nid.jpg"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "submit B: {view}");
    assert_eq!(view["status"], "pending");
    assert_eq!(view["nid_masked"], "******7890");
    assert_eq!(view["trade_license_number"], "TL-2024-9988");

    let uri = format!("/v1/admin/verifications/{store_b}/approve");
    let (status, view) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "tier": "verified", "expected_version": 1 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve verified: {view}");
    assert_eq!(view["status"], "approved");
    assert_eq!(view["tier"], "verified");

    let (status, store) = call(&app, Method::GET, "/v1/stores/wave21-beta", None, None).await;
    assert_eq!(status, StatusCode::OK, "public store B: {store}");
    assert_eq!(store["verified_tier"], "verified");

    // 11. Seller C: rejection carries a note the seller sees;
    // resubmission clears it and returns to pending.
    let seller_c = login(&app, "01744444444").await;
    let store_c = create_store(&app, &seller_c, "wave21-gamma").await;
    let (status, view) = submit_verification(
        &app,
        &seller_c,
        json!({ "nid_number": "12345678901234567" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "submit C: {view}");
    assert_eq!(view["nid_masked"], "*************4567");

    // An empty rejection note is a 400 (the seller needs a reason).
    let uri = format!("/v1/admin/verifications/{store_c}/reject");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "note": "  " })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty note: {body}");

    let note = "NID-র ছবি ঝাপসা — পরিষ্কার ছবি দিয়ে আবার পাঠান";
    let (status, view) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "note": note, "expected_version": 1 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reject C: {view}");
    assert_eq!(view["status"], "rejected");
    assert_eq!(view["tier"], "unverified");

    let (status, view) = call(
        &app,
        Method::GET,
        "/v1/seller/store/verification",
        None,
        Some(&seller_c),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller C view: {view}");
    assert_eq!(view["status"], "rejected");
    assert_eq!(view["reviewer_note"], note);

    let (status, view) = submit_verification(
        &app,
        &seller_c,
        json!({
            "nid_number": "12345678901234567",
            "document_url": "https://example.com/gamma-nid-clear.jpg"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "resubmit C: {view}");
    assert_eq!(view["status"], "pending");
    assert!(view["reviewer_note"].is_null(), "note cleared: {view}");

    // 12. The payout gate. Seller D never verifies; A is basic.
    // Both have payout accounts but no balance.
    let seller_d = login(&app, "01755555555").await;
    create_store(&app, &seller_d, "wave21-delta").await;
    save_payout_account(&app, &seller_d, "01755555555").await;
    save_payout_account(&app, &seller_a, "01722222222").await;

    // Gate defaults OFF: settings say so, and D's request fails
    // on the BALANCE rule — not on verification.
    let (status, settings) = call(&app, Method::GET, "/v1/admin/settings", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "settings: {settings}");
    assert_eq!(settings["kyc_payout_gate"], false);

    let (status, body) = request_payout(&app, &seller_d).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "gate off payout: {body}");
    let message = error_text(&body);
    assert!(message.contains("balance"), "balance error: {body}");
    assert!(
        !message.contains("verification"),
        "no verification error while the gate is off: {body}"
    );

    // Turn the gate ON through the admin settings API.
    let (status, settings) = call(
        &app,
        Method::PUT,
        "/v1/admin/settings",
        Some(json!({ "kyc_payout_gate": true })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "gate on: {settings}");
    assert_eq!(settings["kyc_payout_gate"], true);

    // Unverified D is now stopped by the GATE, with a message
    // that names verification.
    let (status, body) = request_payout(&app, &seller_d).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "gate on payout: {body}");
    assert!(
        error_text(&body).contains("verification"),
        "verification error while the gate is on: {body}"
    );

    // Basic-tier A passes the gate and hits the ordinary balance
    // rule instead — the two failures stay distinguishable.
    let (status, body) = request_payout(&app, &seller_a).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "verified payout: {body}");
    let message = error_text(&body);
    assert!(message.contains("balance"), "balance error: {body}");
    assert!(
        !message.contains("verification"),
        "verified seller must not get the gate error: {body}"
    );

    // Cleanup (Wave 10 lesson): settings rows are global across
    // test binaries — remove the gate row and prove it is off.
    sqlx::query("DELETE FROM platform_settings WHERE key = 'kyc_payout_gate'")
        .execute(&pool)
        .await
        .expect("delete gate setting");
    let (status, settings) = call(&app, Method::GET, "/v1/admin/settings", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "settings after cleanup: {settings}");
    assert_eq!(settings["kyc_payout_gate"], false);

    let (status, body) = request_payout(&app, &seller_d).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "gate off again: {body}");
    assert!(
        error_text(&body).contains("balance"),
        "balance error after gate cleanup: {body}"
    );
}
