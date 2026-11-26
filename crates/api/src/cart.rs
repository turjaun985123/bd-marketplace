//! Buyer carts. One cart per user, created lazily on the first add.
//! All quantity rules come from the pure domain function
//! `validate_cart_quantity` (1..=99) — the cart never re-implements
//! them. Checkout (orders module) reads the same tables; the cart view
//! built here is the single cart representation for every client.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use domain::validate_cart_quantity;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::{ApiError, AppState};

/// One cart line, joined with the product and its store.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct CartItemView {
    pub product_id: Uuid,
    pub store_id: Uuid,
    pub store_slug: String,
    pub title: String,
    pub title_bn: Option<String>,
    pub unit_price_poisha: i64,
    pub quantity: i32,
    pub stock: i32,
}

impl CartItemView {
    /// Line total in poisha (unit price x quantity).
    pub fn line_total_poisha(&self) -> i64 {
        self.unit_price_poisha * i64::from(self.quantity)
    }
}

/// A cart line as serialized: the row fields plus its line total.
#[derive(Debug, Serialize)]
pub struct CartItemJson {
    #[serde(flatten)]
    pub item: CartItemView,
    pub line_total_poisha: i64,
}

/// The full cart response: items plus the goods total (delivery is
/// quoted at checkout, per seller group — not here).
#[derive(Debug, Serialize)]
pub struct CartView {
    pub items: Vec<CartItemJson>,
    pub goods_total_poisha: i64,
}

const CART_ITEMS_SQL: &str = "SELECT ci.product_id, p.store_id, s.slug AS store_slug, \
    p.title, p.title_bn, p.price_poisha AS unit_price_poisha, ci.quantity, p.stock \
    FROM cart_items ci \
    JOIN products p ON p.id = ci.product_id \
    JOIN stores s ON s.id = p.store_id \
    WHERE ci.cart_id = $1 ORDER BY ci.id";

/// Get the user's cart id without creating one.
pub async fn find_cart_id(pool: &PgPool, user_id: Uuid) -> Result<Option<Uuid>, ApiError> {
    sqlx::query_scalar("SELECT id FROM carts WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("find cart", e))
}

/// Get the user's cart id, creating the cart on first use.
pub async fn get_or_create_cart_id(pool: &PgPool, user_id: Uuid) -> Result<Uuid, ApiError> {
    sqlx::query_scalar(
        "INSERT INTO carts (user_id) VALUES ($1) \
         ON CONFLICT (user_id) DO UPDATE SET user_id = EXCLUDED.user_id \
         RETURNING id",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("get or create cart", e))
}

/// Load the cart's items (empty when the user has no cart yet).
pub async fn load_cart_items(pool: &PgPool, user_id: Uuid) -> Result<Vec<CartItemView>, ApiError> {
    let Some(cart_id) = find_cart_id(pool, user_id).await? else {
        return Ok(Vec::new());
    };
    sqlx::query_as(CART_ITEMS_SQL)
        .bind(cart_id)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("load cart items", e))
}

/// THE cart view builder: the GET endpoint and every cart mutation
/// return exactly this shape, built by this one function.
pub async fn cart_view(pool: &PgPool, user_id: Uuid) -> Result<CartView, ApiError> {
    let rows = load_cart_items(pool, user_id).await?;
    let goods_total_poisha = rows.iter().map(CartItemView::line_total_poisha).sum();
    let items = rows
        .into_iter()
        .map(|item| {
            let line_total_poisha = item.line_total_poisha();
            CartItemJson {
                item,
                line_total_poisha,
            }
        })
        .collect();
    Ok(CartView {
        items,
        goods_total_poisha,
    })
}

/// Add `quantity` units of an active product to the cart. Adding a
/// product that is already in the cart accumulates — and the RESULTING
/// quantity must still pass the domain bound (1..=99).
pub async fn add_cart_item(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    quantity: i64,
) -> Result<CartView, ApiError> {
    let add_qty = validate_cart_quantity(quantity).map_err(ApiError::bad_request)?;

    let status: Option<String> = sqlx::query_scalar("SELECT status FROM products WHERE id = $1")
        .bind(product_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load product for cart", e))?;
    match status.as_deref() {
        None => return Err(ApiError::not_found("product not found")),
        Some("active") => {}
        Some(_) => {
            return Err(ApiError::bad_request(
                "only active products can be added to the cart",
            ))
        }
    }

    let cart_id = get_or_create_cart_id(pool, user.user_id).await?;

    let existing: i32 = sqlx::query_scalar(
        "SELECT quantity FROM cart_items WHERE cart_id = $1 AND product_id = $2",
    )
    .bind(cart_id)
    .bind(product_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load cart line", e))?
    .unwrap_or(0);
    let new_qty =
        validate_cart_quantity(i64::from(existing) + i64::from(add_qty)).map_err(ApiError::bad_request)?;

    sqlx::query(
        "INSERT INTO cart_items (cart_id, product_id, quantity) VALUES ($1, $2, $3) \
         ON CONFLICT (cart_id, product_id) DO UPDATE SET quantity = EXCLUDED.quantity",
    )
    .bind(cart_id)
    .bind(product_id)
    .bind(i32::try_from(new_qty).map_err(|_| ApiError::bad_request("quantity out of range"))?)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("add cart item", e))?;

    touch_cart(pool, cart_id).await?;
    cart_view(pool, user.user_id).await
}

/// Set a cart line's quantity. Zero removes the line (the one place a
/// quantity of 0 is meaningful); anything else must pass the domain
/// bound. The product must already be in the cart (404 otherwise).
pub async fn set_cart_item_quantity(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    quantity: i64,
) -> Result<CartView, ApiError> {
    if quantity == 0 {
        return remove_cart_item(pool, user, product_id).await;
    }
    let qty = validate_cart_quantity(quantity).map_err(ApiError::bad_request)?;
    let cart_id = find_cart_id(pool, user.user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("product is not in the cart"))?;

    let result = sqlx::query(
        "UPDATE cart_items SET quantity = $3 WHERE cart_id = $1 AND product_id = $2",
    )
    .bind(cart_id)
    .bind(product_id)
    .bind(i32::try_from(qty).map_err(|_| ApiError::bad_request("quantity out of range"))?)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("set cart item quantity", e))?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("product is not in the cart"));
    }

    touch_cart(pool, cart_id).await?;
    cart_view(pool, user.user_id).await
}

/// Remove one line from the cart (404 when it is not there).
pub async fn remove_cart_item(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
) -> Result<CartView, ApiError> {
    let cart_id = find_cart_id(pool, user.user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("product is not in the cart"))?;

    let result = sqlx::query("DELETE FROM cart_items WHERE cart_id = $1 AND product_id = $2")
        .bind(cart_id)
        .bind(product_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("remove cart item", e))?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("product is not in the cart"));
    }

    touch_cart(pool, cart_id).await?;
    cart_view(pool, user.user_id).await
}

/// Empty the cart (the cart row itself stays — one cart per user).
pub async fn clear_cart(pool: &PgPool, user: &AuthUser) -> Result<CartView, ApiError> {
    if let Some(cart_id) = find_cart_id(pool, user.user_id).await? {
        sqlx::query("DELETE FROM cart_items WHERE cart_id = $1")
            .bind(cart_id)
            .execute(pool)
            .await
            .map_err(|e| ApiError::internal("clear cart", e))?;
        touch_cart(pool, cart_id).await?;
    }
    cart_view(pool, user.user_id).await
}

async fn touch_cart(pool: &PgPool, cart_id: Uuid) -> Result<(), ApiError> {
    sqlx::query("UPDATE carts SET updated_at = now() WHERE id = $1")
        .bind(cart_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("touch cart", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AddCartItemRequest {
    pub product_id: Uuid,
    pub quantity: i64,
}

#[derive(Deserialize)]
pub struct SetCartItemRequest {
    pub quantity: i64,
}

pub async fn add_item(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<AddCartItemRequest>,
) -> Result<(StatusCode, Json<CartView>), ApiError> {
    let pool = state.require_pool()?;
    let view = add_cart_item(pool, &user, req.product_id, req.quantity).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

pub async fn get_cart(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<CartView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(cart_view(pool, user.user_id).await?))
}

pub async fn set_item_quantity(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(req): Json<SetCartItemRequest>,
) -> Result<Json<CartView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        set_cart_item_quantity(pool, &user, product_id, req.quantity).await?,
    ))
}

pub async fn remove_item(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
) -> Result<Json<CartView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(remove_cart_item(pool, &user, product_id).await?))
}

pub async fn clear(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<CartView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(clear_cart(pool, &user).await?))
}
