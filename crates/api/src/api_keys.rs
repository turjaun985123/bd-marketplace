//! Third-party / mobile API keys (Wave 37): long-lived bearer
//! credentials for external apps — a future mobile app, a
//! seller's own integration — that cannot run the phone + OTP
//! login flow.
//!
//! The security model, in one place:
//! - A key belongs to ONE user and acts as that user, narrowed
//!   by its scopes (the pure scope vocabulary and rules are
//!   the domain's Wave 37 section).
//! - The full key (`bdm_live_` + 40 random alphanumeric
//!   characters from the same `rand` CSPRNG that generates
//!   OTP codes) is shown EXACTLY ONCE, in the creation
//!   response. Only its SHA-256 hash is stored — the same
//!   hash-only discipline as OTP codes (`auth::hash_otp`) —
//!   plus a 12-character display prefix so lists can tell
//!   keys apart.
//! - Authentication is centralised: the `AuthUser` extractor
//!   routes a `bdm_live_...` bearer token to
//!   [`authenticate_request`] here, which applies, in order,
//!   the key lookup (revoked / expired → 401), the JWT-only
//!   route exclusions ([`api_key_forbidden_route`] — admin,
//!   payout account + payout requests, subscription
//!   subscribe/cancel, key management itself, data exports),
//!   the scope rule (a read-only key gets 403 on any non-GET
//!   call), and the per-key rate limit (429 past the cap).
//!   No handler re-implements any of it.
//! - The rate limiter is a fixed-window counter held in
//!   `AppState` (in-memory, per process): it resets on
//!   restart and does not coordinate across instances —
//!   documented honestly in the README, like every limit.
//! - `last_used_at` is stamped best-effort AFTER a request
//!   is admitted; a failed stamp never fails the request.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::{
    extract::{Path, State},
    http::{Method, StatusCode},
    Json,
};
use chrono::{DateTime, TimeDelta, Utc};
use domain::{
    api_key_prefix, api_key_scope_allows, validate_api_key_expiry_days, validate_api_key_name,
    validate_api_key_scopes,
};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use uuid::Uuid;

use crate::auth::{ApiKeyContext, AuthUser};
use crate::{ApiError, AppState};

/// Every full key starts with this marker. The `AuthUser`
/// extractor uses it to tell an API key apart from a JWT
/// before either is parsed.
pub const API_KEY_TOKEN_PREFIX: &str = "bdm_live_";

/// Random characters after the marker: 40 alphanumeric
/// characters carry ~238 bits of entropy — far beyond
/// brute force, and the stored hash is all a database leak
/// would expose anyway.
pub const API_KEY_RANDOM_CHARS: usize = 40;

/// Default per-key rate limit: 600 requests per minute
/// (Wave 37). Generous for a mobile app's sync traffic,
/// still a hard cap on a leaked key's blast radius. The
/// `API_KEY_RATE_LIMIT_PER_MIN` startup variable overrides
/// it (see `AppState::from_env`).
pub const DEFAULT_API_KEY_RATE_LIMIT_PER_MIN: u32 = 600;

/// The rate-limit window: one minute, fixed (a window starts
/// at a key's first request inside it and resets wholesale —
/// no sliding-window bookkeeping).
pub const API_KEY_RATE_WINDOW: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Pure helpers (generation, hashing, route rules)
// ---------------------------------------------------------------------------

/// Generate a fresh full API key. The randomness is
/// `rand::thread_rng` — the CSPRNG the OTP generator already
/// uses; no new dependency. The key is returned to the
/// caller once and never stored in this form.
pub fn generate_api_key() -> String {
    let random_part: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(API_KEY_RANDOM_CHARS)
        .map(char::from)
        .collect();
    format!("{API_KEY_TOKEN_PREFIX}{random_part}")
}

/// Hash a full key for storage / lookup: SHA-256, hex —
/// exactly the `auth::hash_otp` discipline. A high-entropy
/// bearer secret needs no salt: the hash exists so a database
/// leak does not leak usable keys.
pub fn hash_api_key(full_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(full_key.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Is this bearer token an API key (rather than a JWT)? The
/// one discriminator the auth layer uses.
pub fn is_api_key_token(token: &str) -> bool {
    token.starts_with(API_KEY_TOKEN_PREFIX)
}

/// Does an HTTP method change state? Everything that is not
/// a safe read (GET / HEAD / OPTIONS) counts as a write for
/// scope purposes.
fn method_is_write(method: &Method) -> bool {
    !matches!(method, &Method::GET | &Method::HEAD | &Method::OPTIONS)
}

/// The JWT-only route exclusions (Wave 37): routes an API
/// key may NEVER use, whatever its scopes. One central list,
/// evaluated in the auth layer — handlers never re-check it.
///
/// - `/v1/admin/*` — platform administration is a human,
///   audited session's work.
/// - `/v1/api-keys*` — key management itself: a key must not
///   mint, list or revoke keys (a leaked key would otherwise
///   be able to persist itself forever).
/// - `/v1/seller/payout-account` (every method) and
///   `POST /v1/seller/payouts` — money out of the platform:
///   reading or changing where payouts go, and requesting
///   one, need a login session. (The payout LIST stays
///   readable — it moves nothing.)
/// - `POST /v1/seller/subscription*` — subscribing and
///   cancelling spend the earnings balance, the same
///   reasoning as payouts. (Reading the current plan and
///   the catalogue stays allowed.)
/// - `/v1/seller/export` and `/v1/buyer/export` — a full
///   data dump of the account is a session-only action.
pub fn api_key_forbidden_route(path: &str, method: &Method) -> bool {
    if path.starts_with("/v1/admin/") || path == "/v1/admin" {
        return true;
    }
    if path.starts_with("/v1/api-keys") {
        return true;
    }
    if path.starts_with("/v1/seller/payout-account") {
        return true;
    }
    if path == "/v1/seller/export" || path == "/v1/buyer/export" {
        return true;
    }
    if method == Method::POST
        && (path == "/v1/seller/payouts" || path.starts_with("/v1/seller/subscription"))
    {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Rate limiter (in-memory fixed windows, one per key)
// ---------------------------------------------------------------------------

/// Per-key fixed-window rate counter. Held in `AppState`
/// behind an `Arc`; windows are keyed by key id and reset
/// wholesale once [`API_KEY_RATE_WINDOW`] has passed since
/// the window opened. Stale entries are swept
/// opportunistically when the map grows large — there is no
/// background task. In-memory by design: a restart resets
/// every window, and multiple API instances would each keep
/// their own count (the README says so, plainly).
#[derive(Debug, Default)]
pub struct ApiKeyRateLimiter {
    windows: Mutex<HashMap<Uuid, (Instant, u32)>>,
}

impl ApiKeyRateLimiter {
    pub fn new() -> Self {
        ApiKeyRateLimiter {
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Admit (and count) one request for `key_id` at `now`,
    /// or refuse it when the key already used its `limit`
    /// inside the current window. `now` is a parameter so
    /// tests can walk the clock; production passes
    /// `Instant::now()` via [`ApiKeyRateLimiter::check`].
    pub fn check_and_record(&self, key_id: Uuid, limit: u32, now: Instant) -> bool {
        let mut windows = self
            .windows
            .lock()
            .expect("api key rate limiter lock poisoned");
        // Opportunistic cleanup: once the map is large, drop
        // windows that have fully expired before adding more.
        if windows.len() > 1024 {
            windows.retain(|_, (started, _)| {
                now.checked_duration_since(*started)
                    .is_some_and(|age| age < API_KEY_RATE_WINDOW)
            });
        }
        let entry = windows.entry(key_id).or_insert((now, 0));
        let expired = now
            .checked_duration_since(entry.0)
            .is_some_and(|age| age >= API_KEY_RATE_WINDOW);
        if expired {
            *entry = (now, 0);
        }
        if entry.1 >= limit {
            return false;
        }
        entry.1 += 1;
        true
    }

    /// [`ApiKeyRateLimiter::check_and_record`] at the real clock.
    pub fn check(&self, key_id: Uuid, limit: u32) -> bool {
        self.check_and_record(key_id, limit, Instant::now())
    }
}

// ---------------------------------------------------------------------------
// Rows and views
// ---------------------------------------------------------------------------

#[derive(Debug, FromRow)]
struct ApiKeyRow {
    id: Uuid,
    name: String,
    key_prefix: String,
    scopes: String,
    last_used_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

const API_KEY_COLUMNS: &str = "id, name, key_prefix, scopes, \
    last_used_at, expires_at, revoked_at, created_at";

/// Split the stored comma-joined scope set back into a list.
fn split_scopes(stored: &str) -> Vec<String> {
    stored
        .split(',')
        .map(|scope| scope.trim().to_string())
        .filter(|scope| !scope.is_empty())
        .collect()
}

/// A key as its owner may see it after creation: metadata and
/// the display prefix — NEVER the full key, never the hash.
#[derive(Debug, Serialize)]
pub struct ApiKeyView {
    pub id: Uuid,
    pub name: String,
    pub key_prefix: String,
    pub scopes: Vec<String>,
    /// active | revoked | expired (derived, never stored).
    pub status: String,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl ApiKeyRow {
    fn view(&self, now: DateTime<Utc>) -> ApiKeyView {
        let status = if self.revoked_at.is_some() {
            "revoked"
        } else if self.expires_at.is_some_and(|expiry| expiry <= now) {
            "expired"
        } else {
            "active"
        };
        ApiKeyView {
            id: self.id,
            name: self.name.clone(),
            key_prefix: self.key_prefix.clone(),
            scopes: split_scopes(&self.scopes),
            status: status.to_string(),
            last_used_at: self.last_used_at,
            expires_at: self.expires_at,
            created_at: self.created_at,
        }
    }
}

/// The creation response: the key view PLUS the full key —
/// the one and only time the full key ever leaves the API.
#[derive(Debug, Serialize)]
pub struct CreatedApiKey {
    #[serde(flatten)]
    pub view: ApiKeyView,
    /// The full key. Shown once; store it now or lose it.
    pub key: String,
}

/// The key list response (`items`, like every list view).
#[derive(Debug, Serialize)]
pub struct ApiKeyList {
    pub items: Vec<ApiKeyView>,
}

// ---------------------------------------------------------------------------
// Service functions (database)
// ---------------------------------------------------------------------------

/// Create a key for the caller: validate through the domain
/// rules, generate the secret, store only its hash, and
/// return the full key ONCE alongside the stored metadata.
pub async fn create_api_key(
    state: &AppState,
    user: &AuthUser,
    name: &str,
    scopes: &[String],
    expires_in_days: Option<i64>,
) -> Result<CreatedApiKey, ApiError> {
    let pool = state.require_pool()?;
    let name = validate_api_key_name(name).map_err(|e| {
        ApiError::bad_request(format!(
            "{e} (API কী-র নাম ১ থেকে ৬০ অক্ষরের মধ্যে হতে হবে)"
        ))
    })?;
    let scopes = validate_api_key_scopes(scopes).map_err(|e| {
        ApiError::bad_request(format!(
            "{e} (স্কোপ শুধু 'read' আর 'write' হতে পারে)"
        ))
    })?;
    let expires_in_days = validate_api_key_expiry_days(expires_in_days).map_err(|e| {
        ApiError::bad_request(format!(
            "{e} (মেয়াদ ১ থেকে ৩৬৫ দিনের মধ্যে হতে হবে, অথবা মেয়াদ ছাড়া রাখুন)"
        ))
    })?;
    let expires_at = expires_in_days.map(|days| Utc::now() + TimeDelta::days(days));

    let full_key = generate_api_key();
    let row: ApiKeyRow = sqlx::query_as(&format!(
        "INSERT INTO api_keys (user_id, name, key_prefix, key_hash, scopes, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {API_KEY_COLUMNS}"
    ))
    .bind(user.user_id)
    .bind(&name)
    .bind(api_key_prefix(&full_key))
    .bind(hash_api_key(&full_key))
    .bind(scopes.join(","))
    .bind(expires_at)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("create api key", e))?;
    Ok(CreatedApiKey {
        view: row.view(Utc::now()),
        key: full_key,
    })
}

/// List the caller's keys, newest first. Metadata and
/// prefixes only — the full key and the hash are not even
/// selected into a view.
pub async fn list_api_keys(state: &AppState, user: &AuthUser) -> Result<ApiKeyList, ApiError> {
    let pool = state.require_pool()?;
    let rows: Vec<ApiKeyRow> = sqlx::query_as(&format!(
        "SELECT {API_KEY_COLUMNS} FROM api_keys \
         WHERE user_id = $1 ORDER BY created_at DESC, id DESC"
    ))
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list api keys", e))?;
    let now = Utc::now();
    Ok(ApiKeyList {
        items: rows.iter().map(|row| row.view(now)).collect(),
    })
}

/// Revoke one of the caller's keys. Revoking an
/// already-revoked key is a 200 with the same view (the
/// operation is idempotent — the desired end state already
/// holds); another user's key, or an unknown id, is a 404,
/// so the endpoint never confirms a stranger's key exists.
pub async fn revoke_api_key(
    state: &AppState,
    user: &AuthUser,
    key_id: Uuid,
) -> Result<ApiKeyView, ApiError> {
    let pool = state.require_pool()?;
    sqlx::query(
        "UPDATE api_keys SET revoked_at = now() \
         WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
    )
    .bind(key_id)
    .bind(user.user_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("revoke api key", e))?;
    let row: Option<ApiKeyRow> = sqlx::query_as(&format!(
        "SELECT {API_KEY_COLUMNS} FROM api_keys WHERE id = $1 AND user_id = $2"
    ))
    .bind(key_id)
    .bind(user.user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load api key", e))?;
    let row = row.ok_or_else(|| {
        ApiError::not_found("api key not found (এই API কীটি পাওয়া যায়নি)")
    })?;
    Ok(row.view(Utc::now()))
}

// ---------------------------------------------------------------------------
// Authentication (called by the AuthUser extractor)
// ---------------------------------------------------------------------------

#[derive(Debug, FromRow)]
struct ApiKeyAuthRow {
    id: Uuid,
    user_id: Uuid,
    scopes: String,
    expires_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    phone: String,
    role: String,
}

/// Look a full key up by its hash and return the identity it
/// carries. Revoked and expired keys are 401s, exactly like
/// a bad JWT — from the client's side an unusable credential
/// is an unusable credential. The user's phone and role are
/// re-read from the user row on every call, so account
/// changes apply to keys immediately.
pub async fn authenticate_api_key(
    state: &AppState,
    full_key: &str,
) -> Result<(AuthUser, Uuid, Vec<String>), ApiError> {
    let pool = state.require_pool()?;
    let row: Option<ApiKeyAuthRow> = sqlx::query_as(
        "SELECT k.id, k.user_id, k.scopes, k.expires_at, k.revoked_at, \
                u.phone, u.role \
         FROM api_keys k JOIN users u ON u.id = k.user_id \
         WHERE k.key_hash = $1",
    )
    .bind(hash_api_key(full_key))
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("look up api key", e))?;
    let row = row.ok_or_else(|| ApiError::unauthorized("invalid api key"))?;
    if row.revoked_at.is_some() {
        return Err(ApiError::unauthorized("api key has been revoked"));
    }
    if row.expires_at.is_some_and(|expiry| expiry <= Utc::now()) {
        return Err(ApiError::unauthorized("api key has expired"));
    }
    let scopes = split_scopes(&row.scopes);
    let user = AuthUser {
        user_id: row.user_id,
        phone: row.phone,
        role: row.role,
        api_key: Some(ApiKeyContext {
            key_id: row.id,
            scopes: scopes.clone(),
        }),
    };
    Ok((user, row.id, scopes))
}

/// The ONE key-authentication entry point, called by the
/// `AuthUser` extractor when the bearer token carries the
/// key marker: credential lookup, then the JWT-only route
/// exclusions, then the scope rule, then the rate limit —
/// and only then is `last_used_at` stamped (best-effort: a
/// failed stamp is logged and forgotten, never a failed
/// request).
pub async fn authenticate_request(
    state: &AppState,
    method: &Method,
    path: &str,
    full_key: &str,
) -> Result<AuthUser, ApiError> {
    let (user, key_id, scopes) = authenticate_api_key(state, full_key).await?;
    if api_key_forbidden_route(path, method) {
        return Err(ApiError::forbidden(
            "this route needs a login session — API keys cannot use it \
             (এই কাজের জন্য API কী চলবে না — লগইন সেশন লাগবে)",
        ));
    }
    if !api_key_scope_allows(&scopes, method_is_write(method)) {
        return Err(ApiError::forbidden(
            "this api key is read-only — it cannot make changes \
             (এই API কী দিয়ে শুধু দেখা যাবে, পরিবর্তন করা যাবে না)",
        ));
    }
    if !state
        .api_key_rate_limiter
        .check(key_id, state.api_key_rate_limit_per_min)
    {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "api key rate limit exceeded — slow down and retry shortly \
             (অনুরোধের সীমা ছাড়িয়ে গেছে — কিছুক্ষণ পরে আবার চেষ্টা করুন)",
        ));
    }
    if let Some(pool) = state.pool.as_ref() {
        if let Err(e) = sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1")
            .bind(key_id)
            .execute(pool)
            .await
        {
            eprintln!("warning: failed to stamp api key last_used_at: {e}");
        }
    }
    Ok(user)
}

// ---------------------------------------------------------------------------
// Handlers (thin: parse -> call one service function -> respond)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    /// Defaults to read-only when omitted — the least
    /// privilege a new key can start with.
    #[serde(default = "default_create_scopes")]
    pub scopes: Vec<String>,
    /// Days until the key expires (1..=365), or null/absent
    /// for a key that does not expire.
    pub expires_in_days: Option<i64>,
}

fn default_create_scopes() -> Vec<String> {
    vec![domain::API_KEY_SCOPE_READ.to_string()]
}

pub async fn create_api_key_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<(StatusCode, Json<CreatedApiKey>), ApiError> {
    let created = create_api_key(&state, &user, &req.name, &req.scopes, req.expires_in_days).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

pub async fn list_api_keys_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<ApiKeyList>, ApiError> {
    Ok(Json(list_api_keys(&state, &user).await?))
}

pub async fn revoke_api_key_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key_id): Path<Uuid>,
) -> Result<Json<ApiKeyView>, ApiError> {
    Ok(Json(revoke_api_key(&state, &user, key_id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_have_the_marker_and_full_length() {
        let key = generate_api_key();
        assert!(key.starts_with(API_KEY_TOKEN_PREFIX), "{key}");
        assert_eq!(
            key.chars().count(),
            API_KEY_TOKEN_PREFIX.chars().count() + API_KEY_RANDOM_CHARS
        );
        assert!(key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'));
        // Two generations differ (a collision would mean the
        // CSPRNG is broken, not bad luck).
        assert_ne!(key, generate_api_key());
        assert!(is_api_key_token(&key));
        assert!(!is_api_key_token("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"));
    }

    #[test]
    fn key_hash_is_stable_hex_and_key_sensitive() {
        let key = generate_api_key();
        let hash = hash_api_key(&key);
        assert_eq!(hash, hash_api_key(&key));
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(hash, hash_api_key(&generate_api_key()));
        // The stored form never contains the key itself.
        assert!(!hash.contains(&key));
    }

    #[test]
    fn forbidden_routes_cover_the_jwt_only_set() {
        // Admin: every method.
        assert!(api_key_forbidden_route("/v1/admin/users", &Method::GET));
        assert!(api_key_forbidden_route(
            "/v1/admin/payouts/x/approve",
            &Method::POST
        ));
        // Key management itself.
        assert!(api_key_forbidden_route("/v1/api-keys", &Method::POST));
        assert!(api_key_forbidden_route("/v1/api-keys", &Method::GET));
        assert!(api_key_forbidden_route("/v1/api-keys/abc", &Method::DELETE));
        // Payout account: reads AND writes.
        assert!(api_key_forbidden_route(
            "/v1/seller/payout-account",
            &Method::GET
        ));
        assert!(api_key_forbidden_route(
            "/v1/seller/payout-account",
            &Method::PUT
        ));
        // Payout requests are POST-only routes; the list stays open.
        assert!(api_key_forbidden_route("/v1/seller/payouts", &Method::POST));
        assert!(!api_key_forbidden_route("/v1/seller/payouts", &Method::GET));
        // Subscription spend: subscribe + cancel are POSTs.
        assert!(api_key_forbidden_route(
            "/v1/seller/subscription",
            &Method::POST
        ));
        assert!(api_key_forbidden_route(
            "/v1/seller/subscription/cancel",
            &Method::POST
        ));
        assert!(!api_key_forbidden_route(
            "/v1/seller/subscription",
            &Method::GET
        ));
        assert!(!api_key_forbidden_route(
            "/v1/seller/subscription/plans",
            &Method::GET
        ));
        // Exports.
        assert!(api_key_forbidden_route("/v1/seller/export", &Method::GET));
        assert!(api_key_forbidden_route("/v1/buyer/export", &Method::GET));
        // Ordinary traffic stays open.
        assert!(!api_key_forbidden_route("/v1/auth/me", &Method::GET));
        assert!(!api_key_forbidden_route("/v1/cart/items", &Method::POST));
        assert!(!api_key_forbidden_route("/v1/orders", &Method::POST));
        assert!(!api_key_forbidden_route(
            "/v1/seller/balance",
            &Method::GET
        ));
    }

    #[test]
    fn rate_limiter_admits_up_to_the_limit_then_refuses() {
        let limiter = ApiKeyRateLimiter::new();
        let key = Uuid::new_v4();
        let start = Instant::now();
        assert!(limiter.check_and_record(key, 3, start));
        assert!(limiter.check_and_record(key, 3, start));
        assert!(limiter.check_and_record(key, 3, start));
        assert!(!limiter.check_and_record(key, 3, start));
        // Still refused inside the window…
        assert!(!limiter.check_and_record(
            key,
            3,
            start + Duration::from_secs(59)
        ));
        // …and admitted again once the window has passed.
        assert!(limiter.check_and_record(
            key,
            3,
            start + API_KEY_RATE_WINDOW
        ));
        // A different key has its own window.
        let other = Uuid::new_v4();
        assert!(limiter.check_and_record(other, 3, start));
    }
}
