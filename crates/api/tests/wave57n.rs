//! Wave 57 integration test: the demo seed's store
//! dressing and its failure isolation.
//! - each seeded store carries its catalogue branding
//!   (theme, brand colour, announcement) — set through
//!   the branding service, visible on the store row;
//! - when one demo slug is already owned by somebody
//!   else, that store's seed outcome is an ERROR and the
//!   other seven stores still seed completely — the seed
//!   never takes over a store and never lets one bad
//!   slug stop the catalogue.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

use api::{run_migrations, AppState};
use courier::CouriersConfig;
use sqlx::PgPool;

/// Build the test state (dev couriers on), or None when
/// no database is configured. Truncates every table
/// EXCEPT `categories` (shared seed — see wave56.rs).
async fn test_state() -> Option<(AppState, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 57 integration tests");
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
         buyer_addresses, bulk_imports, \
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
    let state = AppState::new(Some(pool.clone()), "wave57-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((state, pool))
}

#[tokio::test]
async fn wave57n_seed_branding_and_slug_collision_isolation() {
    let Some((state, pool)) = test_state().await else {
        return;
    };

    // A squatter already owns the agriculture demo slug.
    let squatter_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO users (phone, name, role) VALUES ('+8801799999999', 'Squatter', 'seller') \
         RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .expect("squatter user");
    sqlx::query(
        "INSERT INTO stores (owner_user_id, slug, name) \
         VALUES ($1, 'krishi-bangla-demo', 'Squatted Store')",
    )
    .bind(squatter_id)
    .execute(&pool)
    .await
    .expect("squatter store");

    let report = api::demo_seed::run_demo_seed(&state)
        .await
        .expect("seed run completes despite the collision");
    assert_eq!(report.stores.len(), 8, "every store reports: {report:?}");
    let agro = report
        .stores
        .iter()
        .find(|store| store.store_slug == "krishi-bangla-demo")
        .expect("agro outcome");
    assert!(agro.error.is_some(), "the collision is an error outcome: {agro:?}");
    assert_eq!(agro.products_created, 0);
    let others: Vec<_> = report
        .stores
        .iter()
        .filter(|store| store.store_slug != "krishi-bangla-demo")
        .collect();
    assert_eq!(others.len(), 7);
    for store in &others {
        assert!(store.error.is_none(), "{}: {:?}", store.store_slug, store.error);
        assert!(store.products_created > 0, "{} created products", store.store_slug);
    }
    assert_eq!(report.stores_created, 7, "seven stores seeded around the collision");

    // The squatter's store gained NO products and kept
    // its own name — the seed did not take it over.
    let squatter_products: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM products p JOIN stores s ON s.id = p.store_id \
         WHERE s.slug = 'krishi-bangla-demo'",
    )
    .fetch_one(&pool)
    .await
    .expect("squatter products");
    assert_eq!(squatter_products, 0);

    // Branding landed on a seeded store (the fashion
    // store's catalogue dressing: classic theme, its
    // brand colour, its announcement).
    let (theme, color, announcement): (String, Option<String>, Option<String>) =
        sqlx::query_as(
            "SELECT theme_key, brand_color, announcement FROM stores \
             WHERE slug = 'dhaka-fashion-demo'",
        )
        .fetch_one(&pool)
        .await
        .expect("fashion store branding");
    assert_eq!(theme, "classic");
    assert_eq!(color.as_deref(), Some("#8E244D"));
    assert!(
        announcement
            .as_deref()
            .unwrap_or("")
            .contains("ঈদ কালেকশনে"),
        "announcement: {announcement:?}"
    );

    // The seeded stores' sellers are real seller accounts
    // on the demo numbers.
    let demo_sellers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE phone LIKE '+880171000000%' AND role = 'seller'",
    )
    .fetch_one(&pool)
    .await
    .expect("demo sellers");
    assert_eq!(
        demo_sellers, 8,
        "every store's seller account was ensured (agro's store, not its account, was blocked)"
    );
}
