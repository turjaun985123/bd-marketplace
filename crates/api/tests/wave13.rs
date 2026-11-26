//! Wave 13 integration test: buyer <-> store messaging and the
//! SIDRA bridge. On a bridge-configured app (test secret, notify
//! URL pointing at a dead port — the best-effort notify must not
//! disturb posting): find-or-create returns the same conversation
//! twice; unread counts follow the read markers; a stranger gets
//! 404s; a wrongly-signed SIDRA callback is a 401 that stores
//! nothing; a valid callback with auto-reply OFF becomes a
//! pending suggestion the seller sends into the thread as an `ai`
//! message (and a second one is dismissed); with auto-reply ON a
//! valid callback lands directly; a closed conversation rejects
//! messages and callbacks, and find-or-create then starts fresh.
//! On an unconfigured app the callback is a 503 while human
//! messaging still works. Driven through the real Axum router
//! with tower oneshot; the small helpers are copied from the
//! Wave 12 test on purpose: integration test files stay
//! self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

use api::messaging::sign_sidra_callback;
use api::{build_app, run_migrations, AppState, SidraConfig};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

const SIDRA_SECRET: &str = "wave13-sidra-secret";

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
    assert_eq!(body["sidra_auto_reply"], false, "auto-reply off: {body}");
    body["id"].as_str().expect("store id").to_string()
}

/// Create a product in a store; returns the product id.
async fn create_product(app: &Router, token: &str, store_id: &str, title: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": 120_000,
            "stock": 5,
            "status": "active"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// Open (or re-open) a conversation; returns (status, body).
async fn open_conversation(
    app: &Router,
    token: &str,
    store_id: &str,
    product_id: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = json!({ "store_id": store_id });
    if let Some(product_id) = product_id {
        body["product_id"] = json!(product_id);
    }
    call(app, Method::POST, "/v1/conversations", Some(body), Some(token)).await
}

/// Post a message; returns (status, body).
async fn post_message(
    app: &Router,
    token: &str,
    conversation_id: &str,
    text: &str,
) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        &format!("/v1/conversations/{conversation_id}/messages"),
        Some(json!({ "body": text })),
        Some(token),
    )
    .await
}

/// Read a thread; returns (status, body).
async fn read_thread(app: &Router, token: &str, conversation_id: &str) -> (StatusCode, Value) {
    call(
        app,
        Method::GET,
        &format!("/v1/conversations/{conversation_id}"),
        None,
        Some(token),
    )
    .await
}

/// The caller's conversation list.
async fn list_conversations(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/conversations", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "list conversations: {body}");
    body
}

/// Deliver one SIDRA callback with the given signing secret.
async fn sidra_reply(
    app: &Router,
    secret: &str,
    conversation_id: &str,
    in_reply_to: &str,
    reply_text: &str,
) -> (StatusCode, Value) {
    let signature = sign_sidra_callback(secret, conversation_id, in_reply_to, reply_text);
    call(
        app,
        Method::POST,
        "/v1/sidra/reply",
        Some(json!({
            "conversation_id": conversation_id,
            "in_reply_to_message_id": in_reply_to,
            "reply_text": reply_text,
            "signature": signature
        })),
        None,
    )
    .await
}

#[tokio::test]
async fn wave13_messaging_and_sidra_bridge_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 13 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE sidra_suggestions, messages, conversations, \
         platform_settings, payouts, payout_accounts, review_replies, reviews, \
         shipment_events, shipments, payment_events, payments, \
         order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // The bridge-configured app: a known secret, and a notify URL
    // on a dead port — buyer posts must succeed anyway (the
    // notification is best-effort and detached).
    let state = AppState::new(Some(pool.clone()), "wave13-test-secret".to_string(), true)
        .with_sidra_config(SidraConfig {
            api_url: Some("http://127.0.0.1:1/sidra-notify".to_string()),
            webhook_secret: Some(SIDRA_SECRET.to_string()),
        });
    let app = build_app(state);

    let seller = login(&app, "01713131313").await;
    let store_id = create_store(&app, &seller, "wave13-store").await;
    let product_id = create_product(&app, &seller, &store_id, "Wave13 Panjabi").await;
    let buyer = login(&app, "01714141414").await;
    let buyer2 = login(&app, "01715151515").await;

    // 1. Find-or-create: first call creates, second returns the
    // same conversation.
    let (status, body) = open_conversation(&app, &buyer, &store_id, None).await;
    assert_eq!(status, StatusCode::CREATED, "create conversation: {body}");
    let conversation_id = body["id"].as_str().expect("conversation id").to_string();
    assert_eq!(body["role"], "buyer");
    assert_eq!(body["status"], "open");
    assert_eq!(body["store"]["id"], store_id.as_str());
    assert_eq!(body["unread_count"], 0);
    let (status, body) = open_conversation(&app, &buyer, &store_id, None).await;
    assert_eq!(status, StatusCode::OK, "find conversation: {body}");
    assert_eq!(body["id"], conversation_id.as_str(), "same conversation");

    // A product-scoped conversation is a different triple.
    let (status, body) = open_conversation(&app, &buyer, &store_id, Some(&product_id)).await;
    assert_eq!(status, StatusCode::CREATED, "product conversation: {body}");
    assert_ne!(body["id"], conversation_id.as_str());
    assert_eq!(body["product_title"], "Wave13 Panjabi");

    // 2. The buyer writes; the seller's list shows one unread.
    let (status, body) = post_message(&app, &buyer, &conversation_id, "আসসালামু আলাইকুম, ডেলিভারি চার্জ কত?").await;
    assert_eq!(status, StatusCode::CREATED, "buyer message: {body}");
    assert_eq!(body["sender_type"], "buyer");
    let buyer_message_id = body["id"].as_str().expect("message id").to_string();

    let list = list_conversations(&app, &seller).await;
    let items = list.as_array().expect("conversation list");
    let seller_view = items
        .iter()
        .find(|c| c["id"] == conversation_id.as_str())
        .expect("seller sees the conversation");
    assert_eq!(seller_view["role"], "seller");
    assert_eq!(seller_view["unread_count"], 1, "one unread: {seller_view}");
    assert!(
        seller_view["last_message_preview"]
            .as_str()
            .expect("preview")
            .starts_with("আসসালামু"),
        "preview shows the message: {seller_view}"
    );

    // 3. Reading the thread clears the seller's unread count.
    let (status, body) = read_thread(&app, &seller, &conversation_id).await;
    assert_eq!(status, StatusCode::OK, "seller thread: {body}");
    assert_eq!(body["conversation"]["unread_count"], 0);
    assert_eq!(body["messages"].as_array().expect("messages").len(), 1);
    let list = list_conversations(&app, &seller).await;
    let seller_view = list
        .as_array()
        .expect("list")
        .iter()
        .find(|c| c["id"] == conversation_id.as_str())
        .expect("conversation listed");
    assert_eq!(seller_view["unread_count"], 0, "read clears unread");

    // 4. The seller replies as a seller.
    let (status, body) = post_message(&app, &seller, &conversation_id, "ওয়ালাইকুম আসসালাম! ঢাকার ভিতরে ৬০ টাকা।").await;
    assert_eq!(status, StatusCode::CREATED, "seller message: {body}");
    assert_eq!(body["sender_type"], "seller");

    // 5. A stranger is not a participant: 404 on read and post.
    let (status, _) = read_thread(&app, &buyer2, &conversation_id).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger cannot read");
    let (status, _) = post_message(&app, &buyer2, &conversation_id, "আমি কে?").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger cannot post");

    // 6. A wrongly-signed SIDRA callback is a 401 and stores
    // nothing: no suggestion, no new message.
    let (status, body) = sidra_reply(
        &app,
        "the-wrong-secret",
        &conversation_id,
        &buyer_message_id,
        "জি, ডেলিভারি চার্জ ৬০ টাকা।",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "bad signature: {body}");
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/sidra/suggestions",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suggestions: {body}");
    assert_eq!(body.as_array().expect("suggestions").len(), 0, "nothing stored");
    let (_, body) = read_thread(&app, &buyer, &conversation_id).await;
    assert_eq!(
        body["messages"].as_array().expect("messages").len(),
        2,
        "thread unchanged after the rejected callback"
    );

    // 7. A valid callback with auto-reply OFF becomes a pending
    // suggestion; the thread does not change yet.
    let reply_text = "জি ভাই, ঢাকার ভিতরে ডেলিভারি চার্জ ৬০ টাকা।";
    let (status, body) = sidra_reply(
        &app,
        SIDRA_SECRET,
        &conversation_id,
        &buyer_message_id,
        reply_text,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "sidra reply: {body}");
    assert_eq!(body["mode"], "suggested");
    let suggestion_id = body["suggestion_id"].as_str().expect("suggestion id").to_string();
    let (_, body) = read_thread(&app, &buyer, &conversation_id).await;
    assert_eq!(
        body["messages"].as_array().expect("messages").len(),
        2,
        "a suggestion is not a message yet"
    );

    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/sidra/suggestions",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suggestions: {body}");
    let pending = body.as_array().expect("suggestions");
    assert_eq!(pending.len(), 1, "one pending suggestion: {body}");
    assert_eq!(pending[0]["id"], suggestion_id.as_str());
    assert_eq!(pending[0]["body"], reply_text);
    assert_eq!(pending[0]["status"], "pending");

    // 8. The seller sends it: the words enter the thread as an
    // `ai` message and the suggestion leaves the queue.
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/sidra/suggestions/{suggestion_id}/send"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "send suggestion: {body}");
    assert_eq!(body["status"], "sent");
    let (_, body) = read_thread(&app, &buyer, &conversation_id).await;
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 3, "the ai reply joined the thread");
    assert_eq!(messages[2]["sender_type"], "ai");
    assert_eq!(messages[2]["body"], reply_text);
    assert!(messages[2]["sender_id"].is_null(), "ai has no user id");

    // Sending a decided suggestion again is a 409.
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/sidra/suggestions/{suggestion_id}/send"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "already decided");

    // 9. A second suggestion can be dismissed instead.
    let (status, body) = post_message(&app, &buyer, &conversation_id, "আর ক্যাশ অন ডেলিভারি হবে?").await;
    assert_eq!(status, StatusCode::CREATED, "buyer follow-up: {body}");
    let follow_up_id = body["id"].as_str().expect("message id").to_string();
    let (status, body) = sidra_reply(
        &app,
        SIDRA_SECRET,
        &conversation_id,
        &follow_up_id,
        "জি, ক্যাশ অন ডেলিভারি হবে।",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "second sidra reply: {body}");
    let suggestion2_id = body["suggestion_id"].as_str().expect("suggestion id").to_string();
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/sidra/suggestions/{suggestion2_id}/dismiss"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dismiss: {body}");
    assert_eq!(body["status"], "dismissed");
    let (_, body) = read_thread(&app, &buyer, &conversation_id).await;
    assert_eq!(
        body["messages"].as_array().expect("messages").len(),
        4,
        "a dismissed suggestion never reaches the thread"
    );

    // 10. Auto-reply ON: the next valid callback lands directly.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/sidra/status",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "sidra status: {body}");
    assert_eq!(body["configured"], true);
    assert_eq!(body["auto_reply"], false);
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/sidra",
        Some(json!({ "auto_reply": true })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "enable auto-reply: {body}");
    assert_eq!(body["sidra_auto_reply"], true);

    let (status, body) = open_conversation(&app, &buyer2, &store_id, None).await;
    assert_eq!(status, StatusCode::CREATED, "buyer2 conversation: {body}");
    let conversation2_id = body["id"].as_str().expect("conversation id").to_string();
    let (status, body) = post_message(&app, &buyer2, &conversation2_id, "এই শাড়িটা কি লাল রঙের আছে?").await;
    assert_eq!(status, StatusCode::CREATED, "buyer2 message: {body}");
    let buyer2_message_id = body["id"].as_str().expect("message id").to_string();
    let (status, body) = sidra_reply(
        &app,
        SIDRA_SECRET,
        &conversation2_id,
        &buyer2_message_id,
        "জি আপু, লাল রঙের আছে।",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "auto-reply callback: {body}");
    assert_eq!(body["mode"], "sent");
    let (_, body) = read_thread(&app, &buyer2, &conversation2_id).await;
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 2, "buyer message + direct ai reply");
    assert_eq!(messages[1]["sender_type"], "ai");
    assert_eq!(messages[1]["body"], "জি আপু, লাল রঙের আছে।");

    // 11. Closing: messages and callbacks are rejected, and
    // find-or-create starts a fresh conversation for the triple.
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/conversations/{conversation_id}/close"),
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "close: {body}");
    assert_eq!(body["status"], "closed");
    let (status, _) = post_message(&app, &buyer, &conversation_id, "আরেকটা প্রশ্ন").await;
    assert_eq!(status, StatusCode::CONFLICT, "closed rejects messages");
    let (status, _) = sidra_reply(
        &app,
        SIDRA_SECRET,
        &conversation_id,
        &buyer_message_id,
        "দেরি হয়ে গেছে।",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "closed rejects callbacks");
    let (status, body) = open_conversation(&app, &buyer, &store_id, None).await;
    assert_eq!(status, StatusCode::CREATED, "fresh conversation: {body}");
    assert_ne!(
        body["id"], conversation_id.as_str(),
        "a closed conversation is not reused"
    );

    // 12. The unconfigured app: the callback is a 503, the status
    // says so, and human messaging still works end to end.
    let plain_state = AppState::new(Some(pool.clone()), "wave13-test-secret".to_string(), true);
    let plain_app = build_app(plain_state);
    let seller2 = login(&plain_app, "01716161616").await;
    let store2_id = create_store(&plain_app, &seller2, "wave13-plain-store").await;
    let buyer3 = login(&plain_app, "01717171717").await;
    let (status, body) = open_conversation(&plain_app, &buyer3, &store2_id, None).await;
    assert_eq!(status, StatusCode::CREATED, "plain conversation: {body}");
    let plain_conversation_id = body["id"].as_str().expect("conversation id").to_string();
    let (status, body) = post_message(&plain_app, &buyer3, &plain_conversation_id, "দাম কত?").await;
    assert_eq!(status, StatusCode::CREATED, "plain message: {body}");
    let plain_message_id = body["id"].as_str().expect("message id").to_string();
    let (status, body) = sidra_reply(
        &plain_app,
        SIDRA_SECRET,
        &plain_conversation_id,
        &plain_message_id,
        "দাম ১২০০ টাকা।",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "unconfigured bridge rejects callbacks: {body}"
    );
    let (status, body) = call(
        &plain_app,
        Method::GET,
        "/v1/seller/sidra/status",
        None,
        Some(&seller2),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "plain sidra status: {body}");
    assert_eq!(body["configured"], false);
    let (status, body) = post_message(&plain_app, &seller2, &plain_conversation_id, "১২০০ টাকা ভাই।").await;
    assert_eq!(status, StatusCode::CREATED, "human reply still works: {body}");
    assert_eq!(body["sender_type"], "seller");
}
