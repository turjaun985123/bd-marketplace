//! Buyer carts. One cart per user, created lazily on the first add.
//! All quantity rules come from the pure domain function
//! `validate_cart_quantity` (1..=99) — the cart never re-implements
//! them. Checkout (orders module) reads the same tables; the cart view
//! built here is the single cart representation for every client.
//!
//! Wave 33: a cart line buys ONE variant of its product
//! (`variant_id`, null for variant-less products) — the line's
//! identity is (product, variant), so two variants of one
//! product coexist as separate lines. A variant line's unit
//! price is the variant's FINAL price (base + delta) and its
//! stock is the variant's stock; adding a product that has
//! active variants without choosing one is a 400, as is
//! choosing a variant that is not an active variant of the
//! product. Like the product itself, the variant is checked
//! for existence and availability at add time; the stock
//! ceiling itself is enforced at checkout, exactly as
//! product stock always was (adds never checked stock).
//!
//! Wave 55: save-for-later + the quantity stock cap. A
//! line's `saved` flag (migration 0059) parks it on the
//! buyer's shelf: the cart view returns saved lines as
//! their own array, checkout locks/totals/clears only
//! active lines, and adding a product whose line sits on
//! the shelf revives it into the active cart. Moving a
//! line is one flag UPDATE (`set_cart_item_saved`) — the
//! line's identity never changes. And setting a line's
//! quantity now respects the REAL stock cap (the line's
//! pool: variant stock for variant lines, product stock
//! otherwise): past it is a bilingual 400, where before
//! the ceiling was only enforced at checkout.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use domain::validate_cart_quantity;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::variants::product_has_active_variants;
use crate::{ApiError, AppState};

/// One cart line, joined with the product and its store.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct CartItemView {
    pub product_id: Uuid,
    /// The variant this line buys (Wave 33); null for a
    /// variant-less product. Part of the line's identity.
    pub variant_id: Option<Uuid>,
    /// The variant's name at read time (Wave 33); null for
    /// variant-less lines. The order snapshots the name at
    /// checkout, so a later rename never rewrites an order.
    pub variant_name: Option<String>,
    pub store_id: Uuid,
    pub store_slug: String,
    pub title: String,
    pub title_bn: Option<String>,
    /// The price one unit costs: the product's price, or the
    /// variant's FINAL price (base + delta) for variant lines.
    pub unit_price_poisha: i64,
    pub quantity: i32,
    /// Whether the line is parked on the save-for-later
    /// shelf (Wave 55). Active cart lines are `false`;
    /// the view's `saved_items` array carries `true`.
    pub saved: bool,
    /// The pool this line draws from: the variant's stock
    /// for variant lines, the product's stock otherwise.
    pub stock: i32,
    /// Weight of one unit in grams (Wave 4 — checkout preview sums it).
    pub weight_grams: i32,
    /// The product's primary image base key (Wave 51) — an
    /// intermediate for `image_url`; never serialized.
    #[sqlx(default)]
    #[serde(skip)]
    pub product_image_key: Option<String>,
    /// The product's primary image external URL (Wave 51)
    /// when it is a hotlink; never serialized.
    #[sqlx(default)]
    #[serde(skip)]
    pub product_image_external_url: Option<String>,
    /// The product's primary image original width (Wave
    /// 51); never serialized.
    #[sqlx(default)]
    #[serde(skip)]
    pub product_image_width: Option<i32>,
    /// The product's primary image display URL (Wave 51),
    /// computed by `load_cart_items` through the media
    /// module's one URL rule — the cart renders the same
    /// thumbnail the catalogue showed. Null when the
    /// product has no image.
    #[sqlx(default)]
    pub image_url: Option<String>,
    /// The product's base price (Wave 52) — an intermediate
    /// for the flash price override in `load_cart_items`;
    /// never serialized.
    #[sqlx(default)]
    #[serde(skip)]
    pub base_price_poisha: i64,
    /// The variant's price delta (Wave 52) — an intermediate
    /// for the flash price override: the flash price replaces
    /// the BASE price, and the delta applies on top of it,
    /// exactly as it applies over the base. Never serialized.
    #[sqlx(default)]
    #[serde(skip)]
    pub variant_delta_poisha: Option<i64>,
    /// What one unit would cost WITHOUT a live flash sale
    /// (Wave 52): the SQL's own unit price (base + variant
    /// delta), captured in `load_cart_items` before the flash
    /// override replaces `unit_price_poisha`. Equal to the
    /// unit price whenever no sale governs the line; the
    /// difference between the two is the buyer's per-unit
    /// saving, which the cart page totals from these fields
    /// alone — no second price source, nothing invented.
    #[sqlx(default)]
    pub list_unit_price_poisha: i64,
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

/// The full cart response: the active lines plus the goods total
/// (delivery is quoted at checkout, per seller group — not here),
/// and the save-for-later shelf (Wave 55) as its own array — saved
/// lines never enter the total and never reach checkout.
#[derive(Debug, Serialize)]
pub struct CartView {
    pub items: Vec<CartItemJson>,
    pub goods_total_poisha: i64,
    pub saved_items: Vec<CartItemJson>,
}

const CART_ITEMS_SQL: &str = "SELECT ci.product_id, ci.variant_id, \
    v.name AS variant_name, p.store_id, s.slug AS store_slug, \
    p.title, p.title_bn, \
    CASE WHEN ci.variant_id IS NULL THEN p.price_poisha \
         ELSE p.price_poisha + v.price_delta_poisha END AS unit_price_poisha, \
    p.price_poisha AS base_price_poisha, \
    v.price_delta_poisha AS variant_delta_poisha, \
    ci.quantity, ci.saved, \
    CASE WHEN ci.variant_id IS NULL THEN p.stock ELSE v.stock END AS stock, \
    p.weight_grams, \
    (SELECT pi.storage_key FROM product_images pi \
      WHERE pi.product_id = p.id \
      ORDER BY pi.is_primary DESC, pi.position ASC, pi.created_at ASC LIMIT 1) \
        AS product_image_key, \
    (SELECT pi.external_url FROM product_images pi \
      WHERE pi.product_id = p.id \
      ORDER BY pi.is_primary DESC, pi.position ASC, pi.created_at ASC LIMIT 1) \
        AS product_image_external_url, \
    (SELECT pi.width FROM product_images pi \
      WHERE pi.product_id = p.id \
      ORDER BY pi.is_primary DESC, pi.position ASC, pi.created_at ASC LIMIT 1) \
        AS product_image_width \
    FROM cart_items ci \
    JOIN products p ON p.id = ci.product_id \
    JOIN stores s ON s.id = p.store_id \
    LEFT JOIN product_variants v ON v.id = ci.variant_id \
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
    let mut rows: Vec<CartItemView> = sqlx::query_as(CART_ITEMS_SQL)
        .bind(cart_id)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("load cart items", e))?;
    // Wave 51: fill each line's image_url from its product's
    // primary image facts (the media module's one URL rule).
    for item in rows.iter_mut() {
        item.image_url = crate::media::product_image_url(
            item.product_image_key.as_deref(),
            item.product_image_external_url.as_deref(),
            item.product_image_width,
        );
        // Capture the pre-sale unit price before the flash
        // override below replaces it (Wave 52 savings display).
        item.list_unit_price_poisha = item.unit_price_poisha;
    }
    // Wave 52: a live flash price replaces the line's BASE
    // price (the one resolution in the merch module — the
    // same answer the catalogue showed and checkout will
    // snapshot). A variant line's delta applies on top of
    // the sale price through the same domain rule that
    // applies it over the base; if that composition ever
    // failed to produce a positive price, the line keeps its
    // base-derived price rather than inventing one.
    if !rows.is_empty() {
        let product_ids: Vec<Uuid> = rows.iter().map(|item| item.product_id).collect();
        let resolved =
            crate::merch::flash_price_map(pool, &product_ids, chrono::Utc::now()).await?;
        for item in rows.iter_mut() {
            if let Some(flash) = resolved.get(&item.product_id) {
                item.unit_price_poisha = match item.variant_delta_poisha {
                    Some(delta) => domain::variant_final_price(flash.sale_price_poisha, delta)
                        .unwrap_or(item.unit_price_poisha),
                    None => flash.sale_price_poisha,
                };
            }
        }
    }
    Ok(rows)
}

/// THE cart view builder: the GET endpoint and every cart mutation
/// return exactly this shape, built by this one function. Wave 55:
/// the loaded lines split by their `saved` flag — the goods total
/// sums the ACTIVE lines only.
pub async fn cart_view(pool: &PgPool, user_id: Uuid) -> Result<CartView, ApiError> {
    let rows = load_cart_items(pool, user_id).await?;
    let goods_total_poisha = rows
        .iter()
        .filter(|item| !item.saved)
        .map(CartItemView::line_total_poisha)
        .sum();
    let mut items = Vec::new();
    let mut saved_items = Vec::new();
    for item in rows {
        let line_total_poisha = item.line_total_poisha();
        let json = CartItemJson {
            item,
            line_total_poisha,
        };
        if json.item.saved {
            saved_items.push(json);
        } else {
            items.push(json);
        }
    }
    Ok(CartView {
        items,
        goods_total_poisha,
        saved_items,
    })
}

/// Add `quantity` units of an active product to the cart. Adding a
/// product that is already in the cart accumulates — and the RESULTING
/// quantity must still pass the domain bound (1..=99).
///
/// Wave 33: `variant_id` picks the variant the line buys. A
/// product with active variants REQUIRES one (a variant-less
/// line could not say which stock pool or final price it
/// means); a supplied variant must be an ACTIVE variant OF
/// THIS PRODUCT. Both failures are 400s with the reason.
pub async fn add_cart_item(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    quantity: i64,
    variant_id: Option<Uuid>,
) -> Result<CartView, ApiError> {
    let add_qty = validate_cart_quantity(quantity).map_err(ApiError::bad_request)?;

    let statuses: Option<(String, String)> = sqlx::query_as(
        "SELECT p.status, s.status FROM products p \
         JOIN stores s ON s.id = p.store_id WHERE p.id = $1",
    )
    .bind(product_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load product for cart", e))?;
    match statuses {
        None => return Err(ApiError::not_found("product not found")),
        Some((product_status, store_status)) => {
            if product_status != "active" {
                return Err(ApiError::bad_request(
                    "only active products can be added to the cart",
                ));
            }
            // Admin stores remake: a SUSPENDED store takes
            // no orders, so its products cannot enter a cart
            // either. The pre-remake check read only the
            // product's own status, so a suspended store's
            // stock could be carted (and then sat in the
            // cart view) until checkout's 409 stopped it —
            // the refusal belongs here, at the door, in the
            // cart module's own 400 style.
            if store_status != "active" {
                return Err(ApiError::bad_request(
                    "স্টোরটি সাময়িকভাবে স্থগিত আছে — this store is temporarily suspended and cannot take orders right now",
                ));
            }
        }
    }

    match variant_id {
        Some(variant_id) => {
            let variant: Option<(Uuid, bool)> = sqlx::query_as(
                "SELECT product_id, is_active FROM product_variants WHERE id = $1",
            )
            .bind(variant_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::internal("load variant for cart", e))?;
            match variant {
                Some((owner_product, true)) if owner_product == product_id => {}
                _ => {
                    return Err(ApiError::bad_request(
                        "the chosen variant is not available for this product",
                    ))
                }
            }
        }
        None => {
            if product_has_active_variants(pool, product_id).await? {
                return Err(ApiError::bad_request(
                    "আকার/রং বেছে নিন — this product has variants; please choose a size/colour",
                ));
            }
        }
    }

    let cart_id = get_or_create_cart_id(pool, user.user_id).await?;

    let existing: i32 = sqlx::query_scalar(
        "SELECT quantity FROM cart_items \
         WHERE cart_id = $1 AND product_id = $2 AND variant_id IS NOT DISTINCT FROM $3",
    )
    .bind(cart_id)
    .bind(product_id)
    .bind(variant_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load cart line", e))?
    .unwrap_or(0);
    let new_qty =
        validate_cart_quantity(i64::from(existing) + i64::from(add_qty)).map_err(ApiError::bad_request)?;

    // Wave 55: the upsert also clears the saved flag — a
    // line parked on the shelf that gets added again
    // revives into the active cart (with the accumulated
    // quantity the checks above just validated).
    sqlx::query(
        "INSERT INTO cart_items (cart_id, product_id, variant_id, quantity) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (cart_id, product_id, variant_id) \
         DO UPDATE SET quantity = EXCLUDED.quantity, saved = false",
    )
    .bind(cart_id)
    .bind(product_id)
    .bind(variant_id)
    .bind(i32::try_from(new_qty).map_err(|_| ApiError::bad_request("quantity out of range"))?)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("add cart item", e))?;

    touch_cart(pool, cart_id).await?;
    cart_view(pool, user.user_id).await
}

/// Set a cart line's quantity. Zero removes the line (the one place a
/// quantity of 0 is meaningful); anything else must pass the domain
/// bound. The line — identified by (product, variant), Wave 33 —
/// must already be in the cart (404 otherwise). A caller that
/// omits the variant addresses the variant-less line, exactly
/// as before this wave.
///
/// Wave 55: the quantity also respects the line's REAL stock
/// cap — the variant's stock for a variant line, the
/// product's stock otherwise (the same CASE the cart read
/// uses). Past the cap is a bilingual 400 naming what IS
/// available; checkout keeps its own lock-time enforcement
/// as the final word (stock can move between the two).
/// Only ACTIVE lines take quantity edits — a saved line
/// must be moved back first (the shelf's own actions are
/// move + remove), so it reads as "not in the cart" here.
pub async fn set_cart_item_quantity(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    quantity: i64,
    variant_id: Option<Uuid>,
) -> Result<CartView, ApiError> {
    if quantity == 0 {
        return remove_cart_item(pool, user, product_id, variant_id).await;
    }
    let qty = validate_cart_quantity(quantity).map_err(ApiError::bad_request)?;
    let cart_id = find_cart_id(pool, user.user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("product is not in the cart"))?;

    let stock: Option<i32> = sqlx::query_scalar(
        "SELECT CASE WHEN ci.variant_id IS NULL THEN p.stock ELSE v.stock END \
         FROM cart_items ci \
         JOIN products p ON p.id = ci.product_id \
         LEFT JOIN product_variants v ON v.id = ci.variant_id \
         WHERE ci.cart_id = $1 AND ci.product_id = $2 \
           AND ci.variant_id IS NOT DISTINCT FROM $3 AND NOT ci.saved",
    )
    .bind(cart_id)
    .bind(product_id)
    .bind(variant_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load cart line stock", e))?;
    let stock = stock.ok_or_else(|| ApiError::not_found("product is not in the cart"))?;
    if i64::from(qty) > i64::from(stock) {
        return Err(ApiError::bad_request(format!(
            "স্টকে মাত্র {stock}টি আছে — only {stock} in stock for this item"
        )));
    }

    let result = sqlx::query(
        "UPDATE cart_items SET quantity = $4 \
         WHERE cart_id = $1 AND product_id = $2 AND variant_id IS NOT DISTINCT FROM $3 \
           AND NOT saved",
    )
    .bind(cart_id)
    .bind(product_id)
    .bind(variant_id)
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

/// Move one cart line between the active cart and the
/// save-for-later shelf (Wave 55): one flag UPDATE on the
/// (product, variant) line. Moving to the state the line
/// is already in is a successful no-op returning the
/// current view — the end state the caller asked for
/// already holds. A line that exists in neither state is
/// a 404.
pub async fn set_cart_item_saved(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    variant_id: Option<Uuid>,
    saved: bool,
) -> Result<CartView, ApiError> {
    let cart_id = find_cart_id(pool, user.user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("product is not in the cart"))?;

    let result = sqlx::query(
        "UPDATE cart_items SET saved = $4 \
         WHERE cart_id = $1 AND product_id = $2 AND variant_id IS NOT DISTINCT FROM $3",
    )
    .bind(cart_id)
    .bind(product_id)
    .bind(variant_id)
    .bind(saved)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("move cart item", e))?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("product is not in the cart"));
    }

    touch_cart(pool, cart_id).await?;
    cart_view(pool, user.user_id).await
}

/// Move EVERY saved line back into the active cart
/// (Wave 55 bulk op): one UPDATE over the shelf. An
/// empty shelf is a successful no-op — the end state
/// the caller asked for already holds.
pub async fn move_all_saved_to_cart(
    pool: &PgPool,
    user: &AuthUser,
) -> Result<CartView, ApiError> {
    if let Some(cart_id) = find_cart_id(pool, user.user_id).await? {
        sqlx::query("UPDATE cart_items SET saved = false WHERE cart_id = $1 AND saved")
            .bind(cart_id)
            .execute(pool)
            .await
            .map_err(|e| ApiError::internal("move all saved to cart", e))?;
        touch_cart(pool, cart_id).await?;
    }
    cart_view(pool, user.user_id).await
}

/// Empty the save-for-later shelf (Wave 55 bulk op):
/// deletes only the saved lines; the active cart is
/// untouched. An empty shelf is a successful no-op.
pub async fn clear_saved_items(pool: &PgPool, user: &AuthUser) -> Result<CartView, ApiError> {
    if let Some(cart_id) = find_cart_id(pool, user.user_id).await? {
        sqlx::query("DELETE FROM cart_items WHERE cart_id = $1 AND saved")
            .bind(cart_id)
            .execute(pool)
            .await
            .map_err(|e| ApiError::internal("clear saved items", e))?;
        touch_cart(pool, cart_id).await?;
    }
    cart_view(pool, user.user_id).await
}

/// Remove one line from the cart (404 when it is not there).
/// The line is identified by (product, variant) — Wave 33.
pub async fn remove_cart_item(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    variant_id: Option<Uuid>,
) -> Result<CartView, ApiError> {
    let cart_id = find_cart_id(pool, user.user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("product is not in the cart"))?;

    let result = sqlx::query(
        "DELETE FROM cart_items \
         WHERE cart_id = $1 AND product_id = $2 AND variant_id IS NOT DISTINCT FROM $3",
    )
    .bind(cart_id)
    .bind(product_id)
    .bind(variant_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("remove cart item", e))?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("product is not in the cart"));
    }

    touch_cart(pool, cart_id).await?;
    cart_view(pool, user.user_id).await
}

/// Empty the ACTIVE cart (the cart row itself stays — one cart per
/// user). Wave 55: saved-for-later lines survive a clear — the
/// shelf is a deliberate keep, and silently deleting it under a
/// "clear cart" button would destroy buyer data the button never
/// mentioned. Saved lines leave through their own remove action.
pub async fn clear_cart(pool: &PgPool, user: &AuthUser) -> Result<CartView, ApiError> {
    if let Some(cart_id) = find_cart_id(pool, user.user_id).await? {
        sqlx::query("DELETE FROM cart_items WHERE cart_id = $1 AND NOT saved")
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
    /// The variant the line buys (Wave 33); omitted/null for
    /// variant-less products.
    #[serde(default)]
    pub variant_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct SetCartItemRequest {
    pub quantity: i64,
    /// Which line, when the product has variant lines
    /// (Wave 33); omitted addresses the variant-less line.
    #[serde(default)]
    pub variant_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct RemoveCartItemQuery {
    /// Which line to remove (Wave 33); omitted addresses the
    /// variant-less line.
    #[serde(default)]
    pub variant_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct MoveCartItemRequest {
    /// Which line to move (Wave 55); omitted addresses the
    /// variant-less line, exactly like the quantity write.
    #[serde(default)]
    pub variant_id: Option<Uuid>,
}

pub async fn add_item(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<AddCartItemRequest>,
) -> Result<(StatusCode, Json<CartView>), ApiError> {
    let pool = state.require_pool()?;
    let view = add_cart_item(pool, &user, req.product_id, req.quantity, req.variant_id).await?;
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
        set_cart_item_quantity(pool, &user, product_id, req.quantity, req.variant_id).await?,
    ))
}

pub async fn remove_item(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Query(query): Query<RemoveCartItemQuery>,
) -> Result<Json<CartView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        remove_cart_item(pool, &user, product_id, query.variant_id).await?,
    ))
}

pub async fn clear(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<CartView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(clear_cart(pool, &user).await?))
}

/// POST /v1/cart/items/:product_id/save — park the line
/// on the save-for-later shelf (Wave 55).
pub async fn save_item(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(req): Json<MoveCartItemRequest>,
) -> Result<Json<CartView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        set_cart_item_saved(pool, &user, product_id, req.variant_id, true).await?,
    ))
}

/// POST /v1/cart/items/:product_id/unsave — move the
/// line back into the active cart (Wave 55).
pub async fn unsave_item(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(req): Json<MoveCartItemRequest>,
) -> Result<Json<CartView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        set_cart_item_saved(pool, &user, product_id, req.variant_id, false).await?,
    ))
}

/// POST /v1/cart/saved/move-all — bring the whole shelf
/// back into the active cart (Wave 55).
pub async fn move_all_saved(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<CartView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(move_all_saved_to_cart(pool, &user).await?))
}

/// DELETE /v1/cart/saved — empty the shelf (Wave 55);
/// the active cart is untouched.
pub async fn clear_saved(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<CartView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(clear_saved_items(pool, &user).await?))
}
