//! Wave 56 integration test: the category taxonomy admin
//! API — CRUD behind the admin guard, the audit funnel
//! (create/update/delete/reorder rows land in the audit
//! log in the same transaction), and the domain guards as
//! HTTP behaviour: duplicate slug 409, depth cap 400,
//! reparent cycle 400, reparent depth overflow 400, delete
//! with children 409, delete with products 409.
//!
//! The taxonomy seed (migration 0061) is shared reference
//! data: this file never truncates `categories` and every
//! category it creates carries the `w56a-` slug prefix, so
//! its assertions are slug-scoped and the seed survives for
//! the other Wave 56 binaries sharing this database.
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
use courier::CouriersConfig;
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

/// Login, THEN promote by SQL (login creates the user row;
/// promoting first would land on zero rows — the Wave 54
/// lesson).
async fn login_admin(app: &Router, pool: &PgPool, phone: &str) -> String {
    let token = login(app, phone).await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = $1")
        .bind(format!("+880{}", &phone[1..]))
        .execute(pool)
        .await
        .expect("promote admin");
    token
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table EXCEPT
/// `categories` — the taxonomy seed is shared reference
/// data (see the module doc).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 56 integration tests");
            return None;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         buyer_addresses, \
         product_images, product_variants, products, store_members, store_follows, \
         stores, otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events, saved_searches, \
         search_queries CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave56-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// POST a store for the token's user; returns the store view.
async fn create_store(app: &Router, token: &str, slug: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body
}

/// POST one active product; returns the product view.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price: i64,
    extra: Value,
) -> (StatusCode, Value) {
    let mut body = json!({
        "store_id": store_id,
        "title": title,
        "price_poisha": price,
        "stock": 5,
        "status": "active"
    });
    if let (Some(target), Some(source)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    call(app, Method::POST, "/v1/products", Some(body), Some(token)).await
}

/// POST a category as the admin; returns (status, body).
async fn create_category(app: &Router, admin: &str, body: Value) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/admin/categories",
        Some(body),
        Some(admin),
    )
    .await
}

/// Find a category's admin view by slug in the admin list.
async fn admin_category(app: &Router, admin: &str, slug: &str) -> Option<Value> {
    let (status, list) = call(app, Method::GET, "/v1/admin/categories", None, Some(admin)).await;
    assert_eq!(status, StatusCode::OK, "admin list: {list}");
    list["items"]
        .as_array()
        .expect("admin items")
        .iter()
        .find(|item| item["slug"] == slug)
        .cloned()
}

#[tokio::test]
async fn wave56_admin_crud_guards_and_audit() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let admin = login_admin(&app, &pool, "01711111111").await;
    let plain = login(&app, "01722222222").await;
    let seller = login(&app, "01733333333").await;

    // 1. The write surface is admin-only: anonymous 401,
    // a plain user 403.
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/admin/categories",
        Some(json!({ "slug": "w56a-nope", "name_en": "Nope" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous create refused");
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/admin/categories",
        Some(json!({ "slug": "w56a-nope", "name_en": "Nope" })),
        Some(&plain),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "plain user create refused");

    // 2. Validation: bad slug, unknown icon and an empty
    // English name are all bilingual 400s.
    for body in [
        json!({ "slug": "Bad Slug", "name_en": "Bad" }),
        json!({ "slug": "w56a-bad-icon", "name_en": "Bad", "icon_key": "uploaded.png" }),
        json!({ "slug": "w56a-no-name", "name_en": "   " }),
    ] {
        let (status, err) = create_category(&app, &admin, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "invalid create: {err}");
    }

    // 3. Build a three-level chain: root -> child ->
    // grandchild, depths 1/2/3 in the admin view.
    let (status, root) = create_category(
        &app,
        &admin,
        json!({ "slug": "w56a-root", "name_en": "W56A Root", "name_bn": "রুট", "icon_key": "grid" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create root: {root}");
    assert_eq!(root["depth"], 1);
    assert_eq!(root["child_count"], 0);
    let root_id = root["id"].as_str().expect("root id").to_string();

    let (status, child) = create_category(
        &app,
        &admin,
        json!({ "slug": "w56a-child", "name_en": "W56A Child", "parent_id": root_id }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create child: {child}");
    assert_eq!(child["depth"], 2);
    let child_id = child["id"].as_str().expect("child id").to_string();

    let (status, grand) = create_category(
        &app,
        &admin,
        json!({ "slug": "w56a-grand", "name_en": "W56A Grand", "parent_id": child_id }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create grandchild: {grand}");
    assert_eq!(grand["depth"], 3);
    let grand_id = grand["id"].as_str().expect("grand id").to_string();

    // 4. The depth cap: a fourth level is a 400.
    let (status, err) = create_category(
        &app,
        &admin,
        json!({ "slug": "w56a-too-deep", "name_en": "Too Deep", "parent_id": grand_id }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "depth cap: {err}");

    // 5. A duplicate slug is a friendly 409.
    let (status, err) = create_category(
        &app,
        &admin,
        json!({ "slug": "w56a-root", "name_en": "Duplicate" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "duplicate slug: {err}");

    // 6. An unknown parent is a 400, not a dangling row.
    let (status, err) = create_category(
        &app,
        &admin,
        json!({
            "slug": "w56a-orphan",
            "name_en": "Orphan",
            "parent_id": "00000000-0000-0000-0000-000000000000"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown parent: {err}");

    // 7. The create landed in the audit log, in the same
    // transaction, with the slug in the detail.
    let (status, audit) = call(
        &app,
        Method::GET,
        "/v1/admin/audit-log?action=category.create",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "audit list: {audit}");
    let create_row = audit["items"]
        .as_array()
        .expect("audit items")
        .iter()
        .find(|row| row["target_id"] == root_id)
        .cloned()
        .expect("category.create audited");
    assert_eq!(create_row["target_type"], "category");
    assert_eq!(create_row["detail"]["slug"], "w56a-root");

    // 8. PATCH renames and re-icons; the audit detail names
    // the CHANGED KEYS only.
    let (status, patched) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/categories/{child_id}"),
        Some(json!({ "name_en": "W56A Child Renamed", "icon_key": "shirt" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch child: {patched}");
    assert_eq!(patched["name_en"], "W56A Child Renamed");
    assert_eq!(patched["icon_key"], "shirt");
    let (status, audit) = call(
        &app,
        Method::GET,
        "/v1/admin/audit-log?action=category.update",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "audit list: {audit}");
    let update_row = audit["items"]
        .as_array()
        .expect("audit items")
        .iter()
        .find(|row| row["target_id"] == child_id)
        .cloned()
        .expect("category.update audited");
    let changed = update_row["detail"]["changed_keys"]
        .as_array()
        .expect("changed keys")
        .iter()
        .map(|key| key.as_str().expect("key").to_string())
        .collect::<Vec<_>>();
    assert!(changed.contains(&"name_en".to_string()), "keys: {changed:?}");
    assert!(changed.contains(&"icon_key".to_string()), "keys: {changed:?}");
    assert!(!changed.contains(&"slug".to_string()), "keys: {changed:?}");

    // 9. Reparenting into a cycle is a 400 (root under its
    // own grandchild), and so is a reparent that pushes
    // the subtree past the depth cap.
    let (status, err) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/categories/{root_id}"),
        Some(json!({ "parent_id": grand_id })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "cycle reparent: {err}");

    let (status, d1) = create_category(
        &app,
        &admin,
        json!({ "slug": "w56a-d1", "name_en": "W56A D1" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create d1: {d1}");
    let d1_id = d1["id"].as_str().expect("d1 id").to_string();
    let (status, d2) = create_category(
        &app,
        &admin,
        json!({ "slug": "w56a-d2", "name_en": "W56A D2", "parent_id": d1_id }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create d2: {d2}");
    let d2_id = d2["id"].as_str().expect("d2 id").to_string();
    // The child subtree is 2 tall; hanging it under d2
    // (depth 2) would reach depth 4.
    let (status, err) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/categories/{child_id}"),
        Some(json!({ "parent_id": d2_id })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "depth reparent: {err}");

    // 10. A legal reparent works: the grandchild (a leaf)
    // moves under d2 and lands at depth 3.
    let (status, moved) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/categories/{grand_id}"),
        Some(json!({ "parent_id": d2_id })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "legal reparent: {moved}");
    assert_eq!(moved["depth"], 3);
    assert_eq!(moved["parent_id"], d2_id);
    // Move it back for the delete steps below.
    let (status, _) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/categories/{grand_id}"),
        Some(json!({ "parent_id": child_id })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reparent back");

    // 11. Delete guards: the root has a child (409), the
    // child has a child (409) — and once a product links
    // to the grandchild, the grandchild is blocked too
    // (409, products), even though it is a leaf.
    let (status, err) = call(
        &app,
        Method::DELETE,
        &format!("/v1/admin/categories/{root_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "delete with children: {err}");

    let store = create_store(&app, &seller, "w56a-store").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let (status, product) = create_product(
        &app,
        &seller,
        &store_id,
        "W56A Linked Product",
        10_000,
        json!({ "category_id": grand_id }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "linked product: {product}");
    let (status, err) = call(
        &app,
        Method::DELETE,
        &format!("/v1/admin/categories/{grand_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "delete with products: {err}");
    assert!(
        err["error"].as_str().expect("error text").contains('1'),
        "the guard names the product count: {err}"
    );

    // 12. The admin view carries the guard's numbers.
    let grand_view = admin_category(&app, &admin, "w56a-grand")
        .await
        .expect("grandchild in admin list");
    assert_eq!(grand_view["product_count"], 1);
    assert_eq!(grand_view["child_count"], 0);
    let root_view = admin_category(&app, &admin, "w56a-root")
        .await
        .expect("root in admin list");
    assert_eq!(root_view["child_count"], 1);

    // 13. A clean leaf deletes, and the delete is audited.
    let (status, leaf) = create_category(
        &app,
        &admin,
        json!({ "slug": "w56a-leaf", "name_en": "W56A Leaf" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create leaf: {leaf}");
    let leaf_id = leaf["id"].as_str().expect("leaf id").to_string();
    let (status, deleted) = call(
        &app,
        Method::DELETE,
        &format!("/v1/admin/categories/{leaf_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete leaf: {deleted}");
    assert_eq!(deleted["deleted"], true);
    assert!(
        admin_category(&app, &admin, "w56a-leaf").await.is_none(),
        "leaf is gone from the admin list"
    );
    let (status, audit) = call(
        &app,
        Method::GET,
        "/v1/admin/audit-log?action=category.delete",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "audit list: {audit}");
    assert!(
        audit["items"]
            .as_array()
            .expect("audit items")
            .iter()
            .any(|row| row["target_id"] == leaf_id),
        "category.delete audited: {audit}"
    );

    // 14. Reorder: two roots swap display order in one
    // call, and the admin list (tree order) reflects it.
    let (status, r1) = create_category(
        &app,
        &admin,
        json!({ "slug": "w56a-r1", "name_en": "W56A R1", "sort_order": 1001 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create r1: {r1}");
    let r1_id = r1["id"].as_str().expect("r1 id").to_string();
    let (status, r2) = create_category(
        &app,
        &admin,
        json!({ "slug": "w56a-r2", "name_en": "W56A R2", "sort_order": 1002 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create r2: {r2}");
    let r2_id = r2["id"].as_str().expect("r2 id").to_string();

    let order_of = |list: &Value| {
        let items = list["items"].as_array().expect("items");
        let pos = |slug: &str| {
            items
                .iter()
                .position(|item| item["slug"] == slug)
                .unwrap_or_else(|| panic!("{slug} listed"))
        };
        (pos("w56a-r1"), pos("w56a-r2"))
    };
    let (status, list) = call(&app, Method::GET, "/v1/admin/categories", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "list before reorder: {list}");
    let (before1, before2) = order_of(&list);
    assert!(before1 < before2, "r1 sorts before r2 initially");

    let (status, reordered) = call(
        &app,
        Method::PUT,
        "/v1/admin/categories/reorder",
        Some(json!({
            "items": [
                { "id": r1_id, "sort_order": 1002 },
                { "id": r2_id, "sort_order": 1001 }
            ]
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reorder: {reordered}");
    let (after1, after2) = order_of(&reordered);
    assert!(after2 < after1, "r2 sorts before r1 after the swap");

    // An unknown id aborts the whole reorder.
    let (status, err) = call(
        &app,
        Method::PUT,
        "/v1/admin/categories/reorder",
        Some(json!({
            "items": [
                { "id": "00000000-0000-0000-0000-000000000000", "sort_order": 1 }
            ]
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "reorder unknown id: {err}");

    // 15. Deleting the d-chain from the leaves up works —
    // the guard is about what is attached, not a lock.
    let (status, d3) = create_category(
        &app,
        &admin,
        json!({ "slug": "w56a-d3", "name_en": "W56A D3", "parent_id": d2_id }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create d3: {d3}");
    let d3_id = d3["id"].as_str().expect("d3 id").to_string();
    for id in [d3_id, d2_id, d1_id] {
        let (status, body) = call(
            &app,
            Method::DELETE,
            &format!("/v1/admin/categories/{id}"),
            None,
            Some(&admin),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "delete chain node {id}: {body}");
    }
}
