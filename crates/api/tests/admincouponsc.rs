//! Admin coupons remake, file C: the rebuilt admin list —
//! envelope + WHOLE-QUEUE summary (never filter-shaped),
//! fail-closed filters, LIKE-escaped code search (a literal
//! `_` in a code is findable; `%` matches nothing), sorts that
//! are total orders, and stable pagination. Fixtures span the
//! lifecycle matrix: live, inactive, expired, exhausted, a
//! store coupon carrying its store's name and slug, and a code
//! with a literal underscore.
//!
//! Gated on `DATABASE_URL`: <redacted>
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

/// Log in, then promote to admin by SQL (login FIRST — the
/// promotion must land on the already-created user row).
async fn make_admin(app: &Router, pool: &PgPool, phone_local: &str, phone_canonical: &str) -> String {
    let token = login(app, phone_local).await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = $1")
        .bind(phone_canonical)
        .execute(pool)
        .await
        .expect("promote admin");
    token
}

/// Create a store for the token's user; returns the store id.
async fn create_store(app: &Router, token: &str, name: &str, slug: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": name, "slug": slug })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body["id"].as_str().expect("store id").to_string()
}

/// GET the admin coupon list with a raw query string; asserts 200.
async fn list_ok(app: &Router, admin: &str, query: &str) -> Value {
    let uri = format!("/v1/admin/coupons?{query}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(admin)).await;
    assert_eq!(status, StatusCode::OK, "list ?{query}: {body}");
    body
}

/// Create one platform coupon through the admin API; returns its id.
async fn create_platform_coupon(app: &Router, admin: &str, body: Value) -> String {
    let code = body["code"].as_str().expect("code in body").to_string();
    let (status, created) = call(app, Method::POST, "/v1/admin/coupons", Some(body), Some(admin)).await;
    assert_eq!(status, StatusCode::CREATED, "create {code}: {created}");
    created["id"].as_str().expect("coupon id").to_string()
}

/// The codes of a list page, in order.
fn codes(list: &Value) -> Vec<String> {
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["code"].as_str().expect("code").to_string())
        .collect()
}

#[tokio::test]
async fn admincoupons_list_canon() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin coupons test C");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, api_keys, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         buyer_addresses, bulk_imports, store_layouts, storefront_events, \
         store_pages, store_posts, store_post_events, \
         store_collections, store_collection_products, \
         product_images, product_variants, products, store_members, store_follows, \
         gift_card_ledger, gift_cards, \
         stores, otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events, saved_searches, \
         search_queries CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(Some(pool.clone()), "admincouponsc-test-secret".to_string(), true);
    let app = build_app(state);

    let admin = make_admin(&app, &pool, "01711111111", "+8801711111111").await;
    let seller = login(&app, "01733333333").await;
    create_store(&app, &seller, "Coupon Store", "coupon-store").await;

    // Fixtures across the lifecycle matrix.
    create_platform_coupon(&app, &admin, json!({ "code": "PCTLIVE", "discount_type": "percent", "discount_value": 10 })).await;
    let fixoff_id = create_platform_coupon(
        &app,
        &admin,
        json!({ "code": "FIXOFF", "discount_type": "fixed", "discount_value": 5000 }),
    )
    .await;
    create_platform_coupon(
        &app,
        &admin,
        json!({ "code": "OLDPAST", "discount_type": "percent", "discount_value": 10, "ends_at": "2020-01-01T00:00:00Z" }),
    )
    .await;
    create_platform_coupon(
        &app,
        &admin,
        json!({ "code": "CAPPED1", "discount_type": "percent", "discount_value": 10, "usage_limit": 1 }),
    )
    .await;
    create_platform_coupon(
        &app,
        &admin,
        json!({ "code": "SAVE_20", "discount_type": "fixed", "discount_value": 2000 }),
    )
    .await;

    let (status, store_coupon) = call(
        &app,
        Method::POST,
        "/v1/seller/coupons",
        Some(json!({ "code": "STORECPN", "discount_type": "fixed", "discount_value": 10000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "store coupon: {store_coupon}");

    // FIXOFF becomes inactive through the admin toggle; CAPPED1
    // is stamped exhausted directly (its one slot is spent).
    let (status, response) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/coupons/{fixoff_id}"),
        Some(json!({ "active": false, "reason": "শেষ হয়ে গেছে — campaign over" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "deactivate FIXOFF: {response}");
    sqlx::query("UPDATE coupons SET used_count = 1 WHERE code = 'CAPPED1'")
        .execute(&pool)
        .await
        .expect("stamp CAPPED1 used");

    // A strict created_at ladder makes the time sorts exact.
    for (code, day) in [
        ("PCTLIVE", "2026-01-01T00:00:00Z"),
        ("FIXOFF", "2026-01-02T00:00:00Z"),
        ("STORECPN", "2026-01-03T00:00:00Z"),
        ("OLDPAST", "2026-01-04T00:00:00Z"),
        ("CAPPED1", "2026-01-05T00:00:00Z"),
        ("SAVE_20", "2026-01-06T00:00:00Z"),
    ] {
        sqlx::query("UPDATE coupons SET created_at = $2::timestamptz WHERE code = $1")
            .bind(code)
            .bind(day)
            .execute(&pool)
            .await
            .expect("backdate coupon");
    }

    // The unfiltered list: envelope + the full summary.
    let all = list_ok(&app, &admin, "").await;
    assert_eq!(all["total"], 6);
    assert_eq!(all["items"].as_array().map(Vec::len), Some(6));
    let summary = &all["summary"];
    assert_eq!(summary["total"], 6);
    assert_eq!(summary["platform_count"], 5);
    assert_eq!(summary["store_count"], 1);
    assert_eq!(summary["active_count"], 5);
    assert_eq!(summary["inactive_count"], 1);
    assert_eq!(summary["live_count"], 3, "PCTLIVE + STORECPN + SAVE_20");
    assert_eq!(summary["scheduled_count"], 0);
    assert_eq!(summary["expired_count"], 1, "OLDPAST");
    assert_eq!(summary["exhausted_count"], 1, "CAPPED1");
    assert_eq!(summary["redemptions_all_time"], 0);
    assert_eq!(summary["gross_discount_poisha_all_time"], 0);

    // Rows carry the derived status and the store identity.
    let by_code = |list: &Value, code: &str| {
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["code"] == code)
            .unwrap_or_else(|| panic!("{code} in list"))
            .clone()
    };
    assert_eq!(by_code(&all, "PCTLIVE")["status"], "live");
    assert_eq!(by_code(&all, "FIXOFF")["status"], "inactive");
    assert_eq!(by_code(&all, "OLDPAST")["status"], "expired");
    assert_eq!(by_code(&all, "CAPPED1")["status"], "exhausted");
    let store_row = by_code(&all, "STORECPN");
    assert_eq!(store_row["store_name"], "Coupon Store");
    assert_eq!(store_row["store_slug"], "coupon-store");
    assert_eq!(by_code(&all, "PCTLIVE")["store_name"], Value::Null);

    // Filters narrow total + items exactly; combinations intersect.
    assert_eq!(list_ok(&app, &admin, "scope=platform").await["total"], 5);
    let store_only = list_ok(&app, &admin, "scope=store").await;
    assert_eq!(store_only["total"], 1);
    assert_eq!(codes(&store_only), ["STORECPN"]);
    assert_eq!(list_ok(&app, &admin, "status=live").await["total"], 3);
    assert_eq!(list_ok(&app, &admin, "status=inactive").await["total"], 1);
    assert_eq!(list_ok(&app, &admin, "status=expired").await["total"], 1);
    assert_eq!(list_ok(&app, &admin, "status=exhausted").await["total"], 1);
    assert_eq!(list_ok(&app, &admin, "status=scheduled").await["total"], 0);
    assert_eq!(list_ok(&app, &admin, "discount_type=fixed").await["total"], 3);
    assert_eq!(list_ok(&app, &admin, "discount_type=percent").await["total"], 3);
    assert_eq!(
        list_ok(&app, &admin, "scope=platform&status=live").await["total"],
        2,
        "PCTLIVE + SAVE_20"
    );

    // Search: substring, case-insensitive; metacharacters are
    // escaped — `%` matches nothing, `_` finds only the literal
    // underscore code.
    assert_eq!(codes(&list_ok(&app, &admin, "q=SAVE").await), ["SAVE_20"]);
    assert_eq!(codes(&list_ok(&app, &admin, "q=save_20").await), ["SAVE_20"]);
    assert_eq!(codes(&list_ok(&app, &admin, "q=_").await), ["SAVE_20"]);
    assert_eq!(list_ok(&app, &admin, "q=%25").await["total"], 0);

    // The summary describes the platform, not the filter.
    let filtered = list_ok(&app, &admin, "scope=platform").await;
    assert_eq!(filtered["summary"], all["summary"], "summary is whole-queue");
    let searched = list_ok(&app, &admin, "q=SAVE").await;
    assert_eq!(searched["summary"], all["summary"], "summary ignores search too");

    // Pagination walks the whole set with no overlap.
    let page1 = list_ok(&app, &admin, "limit=2&page=1").await;
    let page2 = list_ok(&app, &admin, "limit=2&page=2").await;
    let page3 = list_ok(&app, &admin, "limit=2&page=3").await;
    assert_eq!(page1["total"], 6);
    let mut walked = codes(&page1);
    walked.extend(codes(&page2));
    walked.extend(codes(&page3));
    walked.sort();
    assert_eq!(
        walked,
        ["CAPPED1", "FIXOFF", "OLDPAST", "PCTLIVE", "SAVE_20", "STORECPN"]
    );

    // Sorts are total orders.
    let newest = list_ok(&app, &admin, "sort=newest").await;
    assert_eq!(codes(&newest).first().map(String::as_str), Some("SAVE_20"));
    let oldest = list_ok(&app, &admin, "sort=oldest").await;
    assert_eq!(codes(&oldest).first().map(String::as_str), Some("PCTLIVE"));
    let code_asc = list_ok(&app, &admin, "sort=code_asc").await;
    assert_eq!(codes(&code_asc).first().map(String::as_str), Some("CAPPED1"));
    let most_used = list_ok(&app, &admin, "sort=most_used").await;
    assert_eq!(codes(&most_used).first().map(String::as_str), Some("CAPPED1"));
    let value_desc = list_ok(&app, &admin, "sort=value_desc").await;
    assert_eq!(
        codes(&value_desc).first().map(String::as_str),
        Some("STORECPN"),
        "the 10_000 fixed coupon outranks every percent value"
    );

    // Invalid filter values fail closed with a 400 naming the set.
    for (query, needle) in [
        ("scope=bogus", "platform"),
        ("status=bogus", "live"),
        ("discount_type=bogus", "percent"),
        ("sort=bogus", "newest"),
    ] {
        let uri = format!("/v1/admin/coupons?{query}");
        let (status, body) = call(&app, Method::GET, &uri, None, Some(&admin)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "list ?{query}: {body}");
        let message = body["error"].as_str().expect("error message");
        assert!(message.contains(needle), "?{query} error names the set: {message}");
    }
}
