//! Wave 25 integration test: product Q&A — a signed-in buyer
//! asks on a publicly visible product (too-short body 400, draft
//! product 404, own-store product 400), the public list shows the
//! question with a MASKED asker (the full phone appears nowhere
//! in the response), only the product's store owner can answer
//! (another seller gets a 404), re-answering corrects the answer,
//! the seller inbox splits answered/unanswered, hiding removes a
//! question from the public list and from the dashboard's
//! unanswered count while keeping it in the inbox, and admins
//! can hide/unhide too. The small helpers are copied from the
//! Wave 9 test on purpose, since integration test files stay
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

/// Create an active product; returns the product id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": "active"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// Ask a question on a product; returns (status, body).
async fn ask(app: &Router, token: &str, product_id: &str, body: &str) -> (StatusCode, Value) {
    let uri = format!("/v1/products/{product_id}/questions");
    call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "body": body })),
        Some(token),
    )
    .await
}

/// The public question list for a product.
async fn public_questions(app: &Router, product_id: &str) -> Value {
    let uri = format!("/v1/products/{product_id}/questions");
    let (status, body) = call(app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "public questions: {body}");
    body
}

/// One seller-inbox read (`answered` = "false" | "true" | "all").
async fn seller_inbox(app: &Router, token: &str, answered: &str) -> Value {
    let uri = format!("/v1/seller/questions?answered={answered}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "seller inbox {answered}: {body}");
    body
}

/// The dashboard's unanswered-questions counter for a seller.
async fn dashboard_unanswered(app: &Router, token: &str) -> i64 {
    let (status, body) = call(app, Method::GET, "/v1/seller/dashboard", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "dashboard: {body}");
    body["unanswered_questions"]
        .as_i64()
        .expect("unanswered_questions present")
}

fn inbox_contains(inbox: &Value, question_id: &str) -> bool {
    inbox["items"]
        .as_array()
        .map(|items| items.iter().any(|item| item["id"] == question_id))
        .unwrap_or(false)
}

fn inbox_item<'a>(inbox: &'a Value, question_id: &str) -> Option<&'a Value> {
    inbox["items"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == question_id))
}

#[tokio::test]
async fn wave25_product_qna_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 25 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE product_questions, payouts, payout_accounts, review_replies, reviews, \
         shipment_events, shipments, payment_events, payments, \
         order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(Some(pool.clone()), "wave25-test-secret".to_string(), true);
    let app = build_app(state);

    // Seller A owns the product under test; seller B owns another
    // store (the stranger who must not answer); a buyer asks; an
    // admin moderates.
    let seller_a = login(&app, "01799999999").await;
    let store_a = create_store(&app, &seller_a, "wave25-store-a").await;
    let product = create_product(&app, &seller_a, &store_a, "Wave25 Product", 100_000, 10).await;
    let seller_b = login(&app, "01599999999").await;
    create_store(&app, &seller_b, "wave25-store-b").await;
    let buyer = login(&app, "01999999999").await;
    let admin = login(&app, "01411111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801411111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // A draft product in store A — not publicly visible.
    let (status, draft) = call(
        &app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_a,
            "title": "Wave25 Draft",
            "price_poisha": 50_000,
            "stock": 5,
            "status": "draft"
        })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create draft: {draft}");
    let draft_id = draft["id"].as_str().expect("draft id").to_string();

    // 1. Input and visibility rules: a too-short question is a
    // 400, a question on a draft product is a 404, and the store
    // owner cannot ask about their own product (400).
    let (status, body) = ask(&app, &buyer, &product, "ab").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "too-short question: {body}");

    let (status, body) = ask(&app, &buyer, &draft_id, "এটা কবে পাওয়া যাবে?").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "draft product question: {body}");

    let (status, body) = ask(&app, &seller_a, &product, "নিজের পণ্যে নিজে প্রশ্ন?").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "own product question: {body}");

    // 2. The buyer asks a real question. It lands unanswered.
    let (status, q1) = ask(&app, &buyer, &product, "ডেলিভারি পেতে কতদিন লাগবে?").await;
    assert_eq!(status, StatusCode::CREATED, "ask q1: {q1}");
    let q1_id = q1["id"].as_str().expect("question id").to_string();
    assert_eq!(q1["is_answered"], false);
    assert!(q1["answer_body"].is_null());

    // 3. The public list shows it — with the asker MASKED. The
    // buyer's full phone (local or canonical) appears nowhere in
    // the response.
    let list = public_questions(&app, &product).await;
    assert_eq!(list["total"], 1, "public list: {list}");
    assert_eq!(list["items"][0]["id"], q1_id);
    assert_eq!(list["items"][0]["is_answered"], false);
    assert_eq!(list["items"][0]["asker_display"], "019****999");
    let raw = list.to_string();
    assert!(
        !raw.contains("01999999999"),
        "full local phone leaked: {raw}"
    );
    assert!(
        !raw.contains("+8801999999999"),
        "full canonical phone leaked: {raw}"
    );

    // 4. The dashboard counts exactly this one visible unanswered
    // question for seller A, and none for seller B.
    assert_eq!(dashboard_unanswered(&app, &seller_a).await, 1);
    assert_eq!(dashboard_unanswered(&app, &seller_b).await, 0);

    // 5. Another store's seller cannot answer it (404 — the
    // question is not theirs and its existence is not leaked),
    // and an empty answer is a 400 for the owner.
    let uri = format!("/v1/seller/questions/{q1_id}/answer");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "body": "আমি অন্য দোকানের" })),
        Some(&seller_b),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger answer: {body}");

    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "body": "   " })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty answer: {body}");

    // 6. The owner answers; the public list shows the answer.
    let (status, answered) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "body": "২-৩ দিন লাগবে" })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "answer q1: {answered}");
    assert_eq!(answered["is_answered"], true);
    assert_eq!(answered["answer_body"], "২-৩ দিন লাগবে");
    assert!(answered["answered_at"].is_string());

    let list = public_questions(&app, &product).await;
    assert_eq!(list["items"][0]["answer_body"], "২-৩ দিন লাগবে");

    // 7. Re-answering CORRECTS the answer on the same row.
    let (status, corrected) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "body": "সংশোধন: ১-২ দিন লাগবে" })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-answer q1: {corrected}");
    assert_eq!(corrected["id"], q1_id);
    assert_eq!(corrected["answer_body"], "সংশোধন: ১-২ দিন লাগবে");
    let list = public_questions(&app, &product).await;
    assert_eq!(list["total"], 1, "still one question: {list}");
    assert_eq!(list["items"][0]["answer_body"], "সংশোধন: ১-২ দিন লাগবে");

    // 8. The inbox splits: q1 is in answered, not in unanswered;
    // seller B's inbox is empty throughout.
    let inbox = seller_inbox(&app, &seller_a, "false").await;
    assert!(!inbox_contains(&inbox, &q1_id), "unanswered inbox: {inbox}");
    let inbox = seller_inbox(&app, &seller_a, "true").await;
    assert!(inbox_contains(&inbox, &q1_id), "answered inbox: {inbox}");
    let item = inbox_item(&inbox, &q1_id).expect("q1 in answered inbox");
    assert_eq!(item["product"]["id"], product);
    assert_eq!(item["product"]["store_slug"], "wave25-store-a");
    assert_eq!(item["is_hidden"], false);
    let inbox_b = seller_inbox(&app, &seller_b, "all").await;
    assert_eq!(inbox_b["total"], 0, "seller B inbox: {inbox_b}");
    assert_eq!(dashboard_unanswered(&app, &seller_a).await, 0);

    // 9. A second question arrives; the seller hides it. It
    // leaves the public list AND the dashboard count, but stays
    // in the seller inbox (all) marked hidden.
    let (status, q2) = ask(&app, &buyer, &product, "কি কি কালার আছে?").await;
    assert_eq!(status, StatusCode::CREATED, "ask q2: {q2}");
    let q2_id = q2["id"].as_str().expect("question id").to_string();
    assert_eq!(dashboard_unanswered(&app, &seller_a).await, 1);

    let hide_uri = format!("/v1/seller/questions/{q2_id}/hide");
    let (status, hidden) = call(&app, Method::POST, &hide_uri, None, Some(&seller_a)).await;
    assert_eq!(status, StatusCode::OK, "hide q2: {hidden}");
    assert_eq!(hidden["is_hidden"], true);

    let list = public_questions(&app, &product).await;
    assert_eq!(list["total"], 1, "q2 hidden from public: {list}");
    assert_eq!(list["items"][0]["id"], q1_id);
    assert_eq!(dashboard_unanswered(&app, &seller_a).await, 0);

    let inbox = seller_inbox(&app, &seller_a, "all").await;
    let item = inbox_item(&inbox, &q2_id).expect("q2 in all inbox");
    assert_eq!(item["is_hidden"], true);

    // 10. A non-admin cannot use the admin moderation route.
    let admin_hide_uri = format!("/v1/admin/questions/{q2_id}/hide");
    let (status, body) = call(&app, Method::POST, &admin_hide_uri, None, Some(&seller_a)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller on admin route: {body}");

    // 11. The admin unhides q2: it returns to the public list
    // and to the dashboard count. Then the admin hides it again.
    let admin_unhide_uri = format!("/v1/admin/questions/{q2_id}/unhide");
    let (status, unhidden) = call(&app, Method::POST, &admin_unhide_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin unhide q2: {unhidden}");
    assert_eq!(unhidden["is_hidden"], false);
    let list = public_questions(&app, &product).await;
    assert_eq!(list["total"], 2, "q2 public again: {list}");
    assert_eq!(dashboard_unanswered(&app, &seller_a).await, 1);

    let (status, hidden) = call(&app, Method::POST, &admin_hide_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin hide q2: {hidden}");
    assert_eq!(hidden["is_hidden"], true);
    let list = public_questions(&app, &product).await;
    assert_eq!(list["total"], 1, "q2 hidden again: {list}");
    assert_eq!(dashboard_unanswered(&app, &seller_a).await, 0);
}
