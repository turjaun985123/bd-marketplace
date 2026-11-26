//! Store access: who may work in a store, and in what role
//! (Wave 34).
//!
//! Until this wave every seller surface resolved access through
//! ownership alone — `stores.owner_user_id = caller`, or the
//! caller's earliest store via `get_store_by_owner`. Wave 34
//! adds the team: a store's owner can seat `manager`s and
//! `staff` (the `store_members` table, migration 0032), and the
//! permission matrix itself is the domain crate's pure
//! `store_role_can` — this module is only the database side of
//! the question:
//!
//! - [`store_role_on`] / [`store_role`]: the caller's role in
//!   one store — `owner`, the member row's role, or `None` when
//!   they have no relationship with the store at all.
//! - [`seller_access`]: the same answer as an erroring gate —
//!   a caller with no relationship gets the same 404 as a
//!   missing store, so strangers cannot probe which stores
//!   exist (the platform's no-leak convention).
//! - [`require_role`]: the matrix gate over a resolved role —
//!   a 403, bilingual, when the role may not perform the
//!   action. A KNOWN role that lacks the action is a 403 (the
//!   caller can see the store; they just may not do this),
//!   while NO relationship at all stays a 404 upstream.
//! - [`accessible_store_ids`] / [`owned_store_ids`]: the scope
//!   sets the list endpoints filter by — every store the caller
//!   owns (all actions) plus every member store whose role
//!   allows the action.
//!
//! The handler families keep their historical no-access status
//! codes: the 404-flavoured surfaces (products, inventory,
//! variants, Q&A, promotions) map `None` to their usual 404,
//! and the 403-flavoured ones (sub-order status, shipment
//! booking, return decisions) map it to their usual 403. Only
//! the OWNER's behaviour is contractually unchanged — owners
//! resolve to `owner` here and pass every gate, exactly as
//! the old equality checks let them through.

use domain::store_role_can;
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::stores::{get_store_by_id, get_store_by_owner, Store};
use crate::ApiError;

/// The optional `?store_id=` selector the store-scoped
/// seller endpoints accept (Wave 34): present, the request
/// acts on that store under the caller's role in it; absent,
/// it acts on the caller's own (earliest) store, exactly as
/// before this wave.
#[derive(Debug, Deserialize)]
pub struct StoreScopeQuery {
    pub store_id: Option<Uuid>,
}

/// Resolve the store a store-scoped request acts on, with
/// the role the caller holds in it: an explicit `store_id`
/// goes through [`seller_access`] + [`require_role`] for the
/// endpoint's action (404 for strangers, 403 for a role that
/// lacks the action); no `store_id` resolves the caller's
/// own earliest store with role `owner` — the pre-Wave-34
/// behaviour, unchanged. A caller with no store of their own
/// and no `store_id` gets the usual 404 from the owner
/// lookup.
pub async fn resolve_scoped_store(
    pool: &PgPool,
    user_id: Uuid,
    store_id: Option<Uuid>,
    action: &str,
) -> Result<(Store, String), ApiError> {
    match store_id {
        Some(store_id) => {
            let role = require_store_action(pool, user_id, store_id, action).await?;
            let store = get_store_by_id(pool, store_id).await?;
            Ok((store, role))
        }
        None => {
            let store = get_store_by_owner(pool, user_id).await?;
            Ok((store, "owner".to_string()))
        }
    }
}

/// The caller's role in one store, over any executor (the
/// pool for plain reads, a transaction for the guarded writes
/// that check access under their row locks): `owner` when
/// they own the store, the member row's role when they are a
/// member, `None` when they have no relationship with it. A
/// store that does not exist is also `None` here — callers
/// that must distinguish use [`seller_access`], which loads
/// the store first.
pub async fn store_role_on<'e, E>(
    executor: E,
    user_id: Uuid,
    store_id: Uuid,
) -> Result<Option<String>, ApiError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    // One query: the store's owner plus the caller's member
    // role in it (null when they are not a member). A missing
    // store yields no row at all.
    let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT s.owner_user_id, m.role \
         FROM stores s \
         LEFT JOIN store_members m ON m.store_id = s.id AND m.user_id = $2 \
         WHERE s.id = $1",
    )
    .bind(store_id)
    .bind(user_id)
    .fetch_optional(executor)
    .await
    .map_err(|e| ApiError::internal("load store access", e))?;
    let Some((owner_user_id, member_role)) = row else {
        return Ok(None);
    };
    if owner_user_id == user_id {
        return Ok(Some("owner".to_string()));
    }
    Ok(member_role)
}

/// [`store_role_on`] on the pool — the form most callers use.
pub async fn store_role(
    pool: &PgPool,
    user_id: Uuid,
    store_id: Uuid,
) -> Result<Option<String>, ApiError> {
    store_role_on(pool, user_id, store_id).await
}

/// Resolve the caller's role in a store or fail: the store
/// must exist (404 when it does not) and the caller must own
/// it or be a member (the SAME 404 when they are neither, so
/// a stranger cannot tell an existing store from a missing
/// one). Returns the role string for [`require_role`].
pub async fn seller_access(
    pool: &PgPool,
    user_id: Uuid,
    store_id: Uuid,
) -> Result<String, ApiError> {
    // Load first so a missing store and a stranger's store
    // produce the identical error below.
    let _store = get_store_by_id(pool, store_id).await?;
    store_role(pool, user_id, store_id)
        .await?
        .ok_or_else(|| ApiError::not_found("store not found"))
}

/// The matrix gate: may `role` perform `action` in a store?
/// A denial is a 403 with a bilingual explanation — the
/// caller legitimately knows the store (they hold SOME role
/// in it), so unlike a stranger's 404 this one may speak.
pub fn require_role(role: &str, action: &str) -> Result<(), ApiError> {
    if store_role_can(role, action) {
        Ok(())
    } else {
        Err(ApiError::forbidden(format!(
            "your role in this store ('{role}') cannot do this ({action}) — \
             এই স্টোরে আপনার ভূমিকায় ('{role}') এই কাজটি করা যাবে না"
        )))
    }
}

/// [`seller_access`] plus [`require_role`] in one call — the
/// form the store-scoped endpoints use: resolve the role
/// (404 for strangers) and gate it on the action (403 for a
/// role that lacks it). Returns the role for callers that
/// also display it.
pub async fn require_store_action(
    pool: &PgPool,
    user_id: Uuid,
    store_id: Uuid,
    action: &str,
) -> Result<String, ApiError> {
    let role = seller_access(pool, user_id, store_id).await?;
    require_role(&role, action)?;
    Ok(role)
}

/// Every store id the caller may act on for `action`: all
/// stores they OWN (an owner may do everything) plus every
/// store where their member role allows the action. THE
/// scope the cross-store list endpoints (seller sub-orders,
/// questions, returns, inventory, analytics) filter by, so a
/// member sees exactly the stores their role serves — no
/// more, no fewer. The order is unspecified; every caller
/// sorts its own rows for display anyway.
pub async fn accessible_store_ids(
    pool: &PgPool,
    user_id: Uuid,
    action: &str,
) -> Result<Vec<Uuid>, ApiError> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, 'owner' FROM stores WHERE owner_user_id = $1 \
         UNION ALL \
         SELECT m.store_id, m.role FROM store_members m WHERE m.user_id = $1",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load accessible stores", e))?;
    Ok(rows
        .into_iter()
        .filter(|(_id, role)| store_role_can(role, action))
        .map(|(id, _role)| id)
        .collect())
}

/// Every store the caller OWNS, earliest first — the scope
/// the pre-Wave-34 owner-wide reads used, kept for the paths
/// that stay owner-scoped by design (the default dashboard
/// scope, the default export scope).
pub async fn owned_store_ids(pool: &PgPool, user_id: Uuid) -> Result<Vec<Uuid>, ApiError> {
    sqlx::query_scalar(
        "SELECT id FROM stores WHERE owner_user_id = $1 ORDER BY created_at ASC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load owned stores", e))
}
