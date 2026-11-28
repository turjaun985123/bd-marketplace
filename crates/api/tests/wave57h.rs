//! Wave 57 integration test: the demo seed's
//! service-level idempotency. The worker's `seed-demo`
//! subcommand is thin arg glue (plus the DEMO_SEED=1
//! guard) over `api::demo_seed::run_demo_seed`; this test
//! calls that function twice against the test database:
//! - run one creates all eight stores and every catalogue
//!   product (>= 120), each with its generated placeholder
//!   image, and links every product to the taxonomy
//!   (every catalogue slug names a real seed node);
//! - run two creates NOTHING — every product updates in
//!   place through the bulk import's sku idempotency,
//!   the product count is unchanged, and no duplicate
//!   placeholder images appear.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

use api::{build_app, run_migrations, AppState};
use axum::Router;
use courier::CouriersConfig;
use sqlx::PgPool;

/// Build the test state (dev couriers on), or None when
/// no database is configured. Truncates every table
/// EXCEPT `categories` (shared seed — see wave56.rs).
/// The seed runs through service functions, so this file
/// needs the AppState itself, not just the router.
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
async fn wave57h_demo_seed_is_idempotent_at_the_service_level() {
    let Some((state, pool)) = test_state().await else {
        return;
    };
    // The router is never called in this test (the seed
    // is service-level), but building it proves the state
    // is whole; keep the binding used.
    let _router: Router = build_app(state.clone());

    // Every category slug the catalogue names exists in
    // the seeded taxonomy — the catalogue can never point
    // at a node that is not there.
    for entry in api::demo_catalogue::DEMO_CATALOGUE {
        for product in entry.products {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM categories WHERE slug = $1)",
            )
            .bind(product.category_slug)
            .fetch_one(&pool)
            .await
            .expect("slug check");
            assert!(
                exists,
                "catalogue slug {} (product {}) exists in the taxonomy",
                product.category_slug, product.sku
            );
        }
    }
    let expected_products = api::demo_catalogue::demo_product_count();
    assert!(
        expected_products >= 120,
        "the demo catalogue carries 120+ products, got {expected_products}"
    );
    assert_eq!(api::demo_catalogue::DEMO_CATALOGUE.len(), 8, "eight demo stores");

    // --- Run one: everything is created.
    let first = api::demo_seed::run_demo_seed(&state)
        .await
        .expect("first seed run");
    assert_eq!(first.stores_created, 8, "{first:?}");
    assert_eq!(first.stores_reused, 0);
    assert_eq!(first.products_created, expected_products, "{first:?}");
    assert_eq!(first.products_updated, 0);
    assert_eq!(first.products_errored, 0, "no seed row may error: {first:?}");
    assert_eq!(
        first.images_attached, expected_products,
        "every created product gets its placeholder: {first:?}"
    );
    for store in &first.stores {
        assert!(store.error.is_none(), "store {}: {:?}", store.store_slug, store.error);
    }

    let total_products: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM products")
        .fetch_one(&pool)
        .await
        .expect("count products");
    assert_eq!(total_products, expected_products as i64);
    let linked: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE category_id IS NOT NULL")
            .fetch_one(&pool)
            .await
            .expect("count linked");
    assert_eq!(linked, expected_products as i64, "every product is taxonomy-linked");
    let image_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM product_images")
        .fetch_one(&pool)
        .await
        .expect("count images");
    assert_eq!(image_rows, expected_products as i64);

    // --- Run two: everything updates, nothing duplicates.
    let second = api::demo_seed::run_demo_seed(&state)
        .await
        .expect("second seed run");
    assert_eq!(second.stores_created, 0, "{second:?}");
    assert_eq!(second.stores_reused, 8);
    assert_eq!(second.products_created, 0, "re-run creates nothing: {second:?}");
    assert_eq!(second.products_updated, expected_products, "{second:?}");
    assert_eq!(second.products_errored, 0);
    assert_eq!(second.images_attached, 0, "updates do not re-render placeholders");

    let total_products_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM products")
        .fetch_one(&pool)
        .await
        .expect("count products after");
    assert_eq!(total_products_after, expected_products as i64);
    let image_rows_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM product_images")
        .fetch_one(&pool)
        .await
        .expect("count images after");
    assert_eq!(image_rows_after, expected_products as i64, "no duplicate images");
    let store_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stores")
        .fetch_one(&pool)
        .await
        .expect("count stores");
    assert_eq!(store_count, 8);
}
