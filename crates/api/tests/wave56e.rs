//! Wave 56 integration test: taxonomy portability and
//! the merge —
//! - export is slug-keyed (parents by slug, never ids);
//! - import upserts by slug in one transaction: new
//!   slugs create, known slugs update, and a document
//!   with a cycle or an unknown parent aborts with
//!   NOTHING written;
//! - merge moves products (re-syncing synced labels),
//!   re-roots children, deletes the source, refuses a
//!   merge into its own descendant;
//! - a store's specialisation endpoint reports its top
//!   linked categories with paths, from real products;
//! - the admin drill-in lists a subtree's products in
//!   any status (the delete guard's count, inspectable).
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

/// Login, THEN promote by SQL (login creates the user row).
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
/// `categories` (shared seed — see wave56.rs).
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

/// POST one product with extra fields merged in; returns the view.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    extra: Value,
) -> Value {
    let mut body = json!({
        "store_id": store_id,
        "title": title,
        "price_poisha": 10_000,
        "stock": 5,
        "status": "active"
    });
    if let (Some(target), Some(source)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    let (status, created) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(body),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {created}");
    created
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

/// Find a node by slug inside a built tree payload.
fn find_node<'a>(nodes: &'a [Value], slug: &str) -> Option<&'a Value> {
    for node in nodes {
        if node["slug"] == slug {
            return Some(node);
        }
        if let Some(children) = node["children"].as_array() {
            if let Some(found) = find_node(children, slug) {
                return Some(found);
            }
        }
    }
    None
}

/// A seed node's id, read from the public tree endpoint.
async fn seed_id(app: &Router, slug: &str) -> String {
    let (status, body) = call(app, Method::GET, "/v1/categories/tree", None, None).await;
    assert_eq!(status, StatusCode::OK, "tree: {body}");
    let nodes = body["tree"].as_array().expect("tree nodes");
    find_node(nodes, slug)
        .unwrap_or_else(|| panic!("seed node {slug} in the public tree"))
        ["id"]
        .as_str()
        .expect("node id")
        .to_string()
}

#[tokio::test]
async fn wave56_import_export_merge_and_specialties() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let admin = login_admin(&app, &pool, "01711111111").await;
    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "w56e-store").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // 1. Export is slug-keyed: the seed's panjabi row
    // names its parent BY SLUG, roots have no parent.
    let (status, export) = call(
        &app,
        Method::GET,
        "/v1/admin/categories/export",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "export: {export}");
    let rows = export["categories"].as_array().expect("export rows");
    let row_of = |slug: &str| {
        rows.iter()
            .find(|row| row["slug"] == slug)
            .unwrap_or_else(|| panic!("export row {slug}"))
    };
    assert!(row_of("fashion")["parent_slug"].is_null());
    assert_eq!(row_of("panjabi")["parent_slug"], "mens-fashion");
    assert_eq!(row_of("panjabi")["name_en"], "Panjabi");
    assert_eq!(row_of("fashion")["is_featured"], true);

    // 2. Import creates a new branch under an existing
    // parent, parents resolved by slug within the same
    // document.
    let document = json!({
        "categories": [
            { "slug": "w56e-imported", "name_en": "W56E Imported", "parent_slug": "fashion" },
            { "slug": "w56e-kid", "name_en": "W56E Kid", "parent_slug": "w56e-imported" }
        ]
    });
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/admin/categories/import",
        Some(document.clone()),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "import: {report}");
    assert_eq!(report["created"], 2);
    assert_eq!(report["updated"], 0);
    let imported = admin_category(&app, &admin, "w56e-imported")
        .await
        .expect("imported listed");
    assert_eq!(imported["depth"], 2);
    let kid = admin_category(&app, &admin, "w56e-kid")
        .await
        .expect("kid listed");
    assert_eq!(kid["depth"], 3);

    // 3. Re-importing the same document UPDATES in place
    // (upsert): nothing new is created, and a changed
    // name lands.
    let mut renamed = document.clone();
    renamed["categories"][0]["name_en"] = json!("W56E Imported Renamed");
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/admin/categories/import",
        Some(renamed),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-import: {report}");
    assert_eq!(report["created"], 0);
    assert_eq!(report["updated"], 2);
    let imported = admin_category(&app, &admin, "w56e-imported")
        .await
        .expect("imported still listed");
    assert_eq!(imported["name_en"], "W56E Imported Renamed");

    // 4. A cyclic document aborts with NOTHING written:
    // neither slug exists afterwards.
    let (status, err) = call(
        &app,
        Method::POST,
        "/v1/admin/categories/import",
        Some(json!({
            "categories": [
                { "slug": "w56e-x", "name_en": "W56E X", "parent_slug": "w56e-y" },
                { "slug": "w56e-y", "name_en": "W56E Y", "parent_slug": "w56e-x" }
            ]
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "cyclic import: {err}");
    assert!(admin_category(&app, &admin, "w56e-x").await.is_none());
    assert!(admin_category(&app, &admin, "w56e-y").await.is_none());

    // 5. An unknown parent slug aborts the same way.
    let (status, err) = call(
        &app,
        Method::POST,
        "/v1/admin/categories/import",
        Some(json!({
            "categories": [
                { "slug": "w56e-lost", "name_en": "W56E Lost", "parent_slug": "w56e-ghost" }
            ]
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown parent import: {err}");
    assert!(admin_category(&app, &admin, "w56e-lost").await.is_none());

    // 6. A slugless row gets its slug derived from the
    // English name.
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/admin/categories/import",
        Some(json!({
            "categories": [
                { "name_en": "W56E Derived Branch" }
            ]
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "derived import: {report}");
    assert_eq!(report["created"], 1);
    assert!(
        admin_category(&app, &admin, "w56e-derived-branch").await.is_some(),
        "derived slug listed"
    );

    // 7. Merge: a source with a child and a synced
    // product folds into Others — the product re-links
    // AND re-syncs its label, the child re-roots, the
    // source disappears.
    let (status, src) = call(
        &app,
        Method::POST,
        "/v1/admin/categories",
        Some(json!({ "slug": "w56e-src", "name_en": "W56E Src" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "merge source: {src}");
    let src_id = src["id"].as_str().expect("src id").to_string();
    let (status, src_kid) = call(
        &app,
        Method::POST,
        "/v1/admin/categories",
        Some(json!({
            "slug": "w56e-src-kid",
            "name_en": "W56E Src Kid",
            "parent_id": src_id
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "merge source kid: {src_kid}");
    let product = create_product(
        &app,
        &seller,
        &store_id,
        "W56E Merged Product",
        json!({ "category_id": src_id }),
    )
    .await;
    assert_eq!(product["category"], "W56E Src", "synced before merge");
    let product_id = product["id"].as_str().expect("product id").to_string();

    let others_id = seed_id(&app, "others").await;
    let (status, report) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/categories/{src_id}/merge"),
        Some(json!({ "target_id": others_id })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "merge: {report}");
    assert_eq!(report["products_moved"], 1);
    assert_eq!(report["children_moved"], 1);
    assert_eq!(report["target"]["slug"], "others");
    assert!(admin_category(&app, &admin, "w56e-src").await.is_none());
    let kid_after = admin_category(&app, &admin, "w56e-src-kid")
        .await
        .expect("kid survives the merge");
    assert_eq!(kid_after["parent_id"], others_id);

    let (status, after) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "product after merge: {after}");
    assert_eq!(after["product"]["category_id"], others_id);
    assert_eq!(
        after["product"]["category"], "Others",
        "the synced label re-syncs to the target"
    );

    // 8. Merge guards: into itself, and into its own
    // descendant, are both 400s.
    let fashion_id = seed_id(&app, "fashion").await;
    let panjabi_id = seed_id(&app, "panjabi").await;
    let (status, err) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/categories/{fashion_id}/merge"),
        Some(json!({ "target_id": fashion_id })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "self merge: {err}");
    let (status, err) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/categories/{fashion_id}/merge"),
        Some(json!({ "target_id": panjabi_id })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "descendant merge: {err}");

    // 9. Store specialisation: two Panjabi products and
    // one Electronics product make Panjabi the store's
    // top specialty, with its three-entry path.
    create_product(
        &app,
        &seller,
        &store_id,
        "W56E Panjabi One",
        json!({ "category_id": panjabi_id }),
    )
    .await;
    create_product(
        &app,
        &seller,
        &store_id,
        "W56E Panjabi Two",
        json!({ "category_id": panjabi_id }),
    )
    .await;
    let electronics_id = seed_id(&app, "electronics").await;
    create_product(
        &app,
        &seller,
        &store_id,
        "W56E Gadget",
        json!({ "category_id": electronics_id }),
    )
    .await;
    let (status, specialties) = call(
        &app,
        Method::GET,
        "/v1/stores/w56e-store/categories",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "specialties: {specialties}");
    let cats = specialties["categories"].as_array().expect("specialties");
    assert_eq!(cats[0]["slug"], "panjabi", "top specialty: {cats:?}");
    assert_eq!(cats[0]["product_count"], 2);
    let path: Vec<String> = cats[0]["path"]
        .as_array()
        .expect("specialty path")
        .iter()
        .map(|entry| entry["slug"].as_str().expect("slug").to_string())
        .collect();
    assert_eq!(path, vec!["fashion", "mens-fashion", "panjabi"]);
    let gadget = cats
        .iter()
        .find(|row| row["slug"] == "electronics")
        .expect("electronics specialty");
    assert_eq!(gadget["product_count"], 1);
    // The merged product counts under Others too.
    let others = cats
        .iter()
        .find(|row| row["slug"] == "others")
        .expect("others specialty");
    assert_eq!(others["product_count"], 1);
    // An unknown store 404s.
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/stores/w56e-ghost/categories",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "ghost store 404s");

    // 10. The admin drill-in lists the Fashion subtree's
    // products (the two Panjabi products — the merged one
    // now lives under Others and is NOT here).
    let (status, drill) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/categories/{fashion_id}/products"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "drill-in: {drill}");
    assert_eq!(drill["total"], 2, "fashion subtree products: {drill}");
    let drill_titles: Vec<String> = drill["items"]
        .as_array()
        .expect("drill items")
        .iter()
        .map(|item| item["title"].as_str().expect("title").to_string())
        .collect();
    assert!(drill_titles.contains(&"W56E Panjabi One".to_string()));
    assert!(drill_titles.contains(&"W56E Panjabi Two".to_string()));
}
