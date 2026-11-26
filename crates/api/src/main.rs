//! BD Marketplace API binary. All behaviour lives in the `api` library
//! crate (`build_app`, `run`, and the auth/stores/products service
//! modules) so integration tests can drive the same router in-process.

#[tokio::main]
async fn main() {
    api::run().await;
}
