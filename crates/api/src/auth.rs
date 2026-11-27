//! Phone + OTP + JWT auth.
//!
//! Individually callable pieces, composed by the two thin handlers:
//! - `generate_otp` / `hash_otp` — pure OTP helpers (only the HASH of a
//!   code is ever stored, in `otp_codes`).
//! - SMS delivery — the `notify` crate's `SmsSender` (Wave 29 moved
//!   the trait and its senders there, so OTP codes and outbox
//!   notifications share the ONE selection rule,
//!   `notify::sender_from_env`). In production mode with no real
//!   gateway configured, requesting an OTP returns a typed error —
//!   an SMS is never silently "sent" to nowhere.
//! - `issue_token` / `authenticate` — JWT (HS256) issue and verification.
//! - `request_otp_code` / `verify_otp_code` — the service functions that
//!   talk to the database; the handlers below only call them.
//!   Wave 28 adds a per-phone request throttle inside
//!   `request_otp_code`: requests are counted from the append-only
//!   `otp_request_log` table against the domain hourly cap, and
//!   excess requests get a 429 before any code is stored or sent.

use axum::{
    extract::{FromRequestParts, State},
    http::{header::AUTHORIZATION, request::Parts, HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, TimeDelta, Utc};
use domain::{normalize_bd_phone, otp_request_allowed};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::{ApiError, AppState};

/// OTP lifetime and brute-force limits.
pub const OTP_EXPIRY_MINUTES: i64 = 5;
pub const OTP_MAX_ATTEMPTS: i32 = 5;
/// JWT lifetime: 7 days, in seconds.
pub const TOKEN_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Generate a random 6-digit OTP code (leading zeros kept).
pub fn generate_otp() -> String {
    let n: u32 = rand::thread_rng().gen_range(0..1_000_000);
    format!("{n:06}")
}

/// Hash an OTP for storage. The phone is mixed in so the same code on a
/// different phone hashes differently. Plaintext codes are never stored.
pub fn hash_otp(phone: &str, code: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(phone.as_bytes());
    hasher.update(b":");
    hasher.update(code.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ---------------------------------------------------------------------------
// SMS delivery
// ---------------------------------------------------------------------------

/// Pick the SMS sender for this state, or fail honestly: outside
/// dev mode no real gateway is configured, so OTP delivery is a
/// typed 503 — we never pretend an SMS was sent. The sender
/// itself is the `notify` crate's (Wave 29): `sender_from_env`
/// is the one selection rule the API and the worker share, and
/// in dev mode it yields the log sender.
fn sms_sender(state: &AppState) -> Result<Box<dyn notify::SmsSender>, ApiError> {
    if state.sms_dev_mode {
        Ok(notify::sender_from_env(true))
    } else {
        Err(ApiError::service_unavailable(
            "SMS delivery is not configured (no gateway; dev mode is off)",
        ))
    }
}

// ---------------------------------------------------------------------------
// Users + JWT
// ---------------------------------------------------------------------------

/// A row of the `users` table; also the public user JSON shape.
/// `is_admin` (Wave 10) is a flag ON TOP of the role: an admin
/// keeps their buyer/seller role, and the flag alone unlocks the
/// `/v1/admin/*` endpoints (see the `admin` module).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct User {
    pub id: Uuid,
    pub phone: String,
    pub name: Option<String>,
    pub role: String,
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
}

const USER_COLUMNS: &str = "id, phone, name, role, is_admin, created_at";

/// Load one user by id (404 when there is no such user). The admin
/// guard re-reads the row on every call so a revoked admin flag
/// takes effect immediately, not at token expiry.
pub async fn load_user(pool: &PgPool, user_id: Uuid) -> Result<User, ApiError> {
    sqlx::query_as(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = $1"))
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load user", e))?
        .ok_or_else(|| ApiError::not_found("user not found"))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    /// Subject: the user id, as a string.
    pub sub: String,
    pub phone: String,
    pub role: String,
    /// Expiry (unix seconds). Required by the default JWT validation.
    pub exp: usize,
}

/// Issue a signed JWT for a user (HS256, 7-day expiry).
pub fn issue_token(state: &AppState, user: &User) -> Result<String, ApiError> {
    let exp = (Utc::now() + TimeDelta::seconds(TOKEN_TTL_SECONDS)).timestamp();
    let claims = Claims {
        sub: user.id.to_string(),
        phone: user.phone.clone(),
        role: user.role.clone(),
        exp: exp as usize,
    };
    // Wave 47 (jsonwebtoken 12): `from_secret` now returns a
    // `Result` — it fails only on an EMPTY secret
    // (`ErrorKind::InvalidKeyFormat`). That is a server
    // misconfiguration, not a client error, so it takes the
    // same 500 as a signing failure; Wave 28's
    // `validate_jwt_secret` already keeps a real deployment
    // from ever reaching it.
    let key = EncodingKey::from_secret(state.jwt_secret.as_bytes()).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to sign token: {e}"),
        )
    })?;
    encode(&Header::default(), &claims, &key).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to sign token: {e}"),
        )
    })
}

/// How an API-key caller authenticated (Wave 37): the key's
/// id and its stored scopes, carried on the `AuthUser` so the
/// one central auth check (and any future per-handler rule)
/// can see them. JWT callers carry `None` — a login session
/// is never scope-narrowed and never rate limited by the key
/// limiter.
#[derive(Debug, Clone)]
pub struct ApiKeyContext {
    pub key_id: Uuid,
    pub scopes: Vec<String>,
}

/// The authenticated caller, extracted from a Bearer token —
/// a JWT from the OTP login, or (Wave 37) a `bdm_live_...`
/// API key acting as its owner.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub phone: String,
    pub role: String,
    /// Wave 37: `Some` when this request authenticated with
    /// an API key rather than a JWT.
    pub api_key: Option<ApiKeyContext>,
}

/// Verify the `Authorization: Bearer <jwt>` header. Single function used
/// by both the strict extractor (protected endpoints) and the optional
/// variant (public endpoints that behave differently for owners).
/// JWTs only: since Wave 37 the strict extractor routes
/// `bdm_live_...` API keys to the `api_keys` module's
/// authentication before ever calling this, and the optional
/// variant deliberately stays JWT-only (on the public
/// catalogue reads a key caller is simply a guest).
pub fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<AuthUser, ApiError> {
    let value = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("missing Authorization header"))?;
    let token = value
        .strip_prefix("Bearer ")
        .ok_or_else(|| ApiError::unauthorized("Authorization header must be a Bearer token"))?;
    // Wave 47 (jsonwebtoken 12): `from_secret` now returns a
    // `Result` (fails only on an empty secret). It is folded
    // into this function's single 401 shape, like every other
    // verification failure — `authenticate` never distinguished
    // failure causes, and `authenticate_optional` treats any
    // error as "guest".
    let key = DecodingKey::from_secret(state.jwt_secret.as_bytes())
        .map_err(|_| ApiError::unauthorized("invalid or expired token"))?;
    let data = decode::<Claims>(token, &key, &Validation::new(Algorithm::HS256))
        .map_err(|_| ApiError::unauthorized("invalid or expired token"))?;
    let user_id = Uuid::parse_str(&data.claims.sub)
        .map_err(|_| ApiError::unauthorized("token subject is not a user id"))?;
    Ok(AuthUser {
        user_id,
        phone: data.claims.phone,
        role: data.claims.role,
        api_key: None,
    })
}

/// `authenticate` for public endpoints: any problem just means "guest".
pub fn authenticate_optional(state: &AppState, headers: &HeaderMap) -> Option<AuthUser> {
    authenticate(state, headers).ok()
}

#[axum::async_trait]
impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // Wave 37: a bearer token carrying the API-key marker
        // is a key, not a JWT — it goes through the key path
        // (hash lookup, JWT-only route exclusions, scope rule,
        // per-key rate limit), which needs the method and the
        // path, both available here and nowhere else in the
        // auth layer. Everything else is a JWT, exactly as
        // before.
        let bearer = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        if let Some(token) = bearer {
            if crate::api_keys::is_api_key_token(token) {
                return crate::api_keys::authenticate_request(
                    state,
                    &parts.method,
                    parts.uri.path(),
                    token,
                )
                .await;
            }
        }
        authenticate(state, &parts.headers)
    }
}

// ---------------------------------------------------------------------------
// Service functions (database)
// ---------------------------------------------------------------------------

#[derive(FromRow)]
struct OtpRow {
    code_hash: String,
    expires_at: DateTime<Utc>,
}

/// Request an OTP for a phone number: validate + normalize the number,
/// store only the hash of a fresh code, and deliver it via the SMS
/// sender. Returns the canonical phone and, in dev mode only, the code
/// itself (so local testing needs no phone).
pub async fn request_otp_code(
    state: &AppState,
    raw_phone: &str,
) -> Result<(String, Option<String>), ApiError> {
    let pool = state.require_pool()?;
    let phone = normalize_bd_phone(raw_phone)
        .ok_or_else(|| ApiError::bad_request("invalid Bangladeshi phone number"))?;
    // Fail before storing anything if we cannot deliver the code.
    let sender = sms_sender(state)?;

    // Wave 28 throttle: cap how many codes one number may
    // request per rolling hour (the pure rule is the domain's
    // `otp_request_allowed`). The count comes from the
    // append-only `otp_request_log` table — `otp_codes` itself
    // holds at most one row per phone (a new request deletes
    // the previous rows), so it cannot answer "how many
    // requests in the last hour". A refused request is NOT
    // logged: hammering the endpoint costs one COUNT query
    // per attempt and no SMS.
    //
    // Wave 41 (security audit HI-04/F04 + HI-06/F06): the
    // count check, the log insert and the code replacement
    // used to be separate autocommit statements, so two
    // concurrent requests for the same phone could BOTH pass
    // the count check (bursting past the cap) and could
    // interleave their DELETE/INSERT pairs into two live
    // codes. All of it now runs in ONE transaction serialized
    // per phone by a transaction-scoped advisory lock keyed
    // on the phone (there is no per-phone row to lock before
    // the first request exists — the advisory lock is the
    // anchor; it releases at commit/rollback and never leaks
    // across requests). The SMS goes out only AFTER the
    // transaction commits, so a delivered code is always the
    // committed one (audit HI-06's prescription).
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin OTP request transaction", e))?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(&phone)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("lock OTP request for phone", e))?;

    let recent_requests: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM otp_request_log \
         WHERE phone = $1 AND created_at > now() - interval '1 hour'",
    )
    .bind(&phone)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("count recent OTP requests", e))?;
    if !otp_request_allowed(recent_requests) {
        // Rollback drops the transaction unlogged, exactly as
        // before: a refused request writes nothing.
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too many OTP requests for this number — please wait a while and \
             try again (এই নম্বরে অনেকবার OTP চাওয়া হয়েছে — কিছুক্ষণ অপেক্ষা \
             করে আবার চেষ্টা করুন)",
        ));
    }
    sqlx::query("INSERT INTO otp_request_log (phone) VALUES ($1)")
        .bind(&phone)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("log OTP request", e))?;

    let code = generate_otp();
    let code_hash = hash_otp(&phone, &code);
    let expires_at = Utc::now() + TimeDelta::minutes(OTP_EXPIRY_MINUTES);

    // One active code per phone: replace any previous rows —
    // inside the locked transaction, so a concurrent request
    // for the same phone can no longer interleave a second
    // live code between the DELETE and the INSERT.
    sqlx::query("DELETE FROM otp_codes WHERE phone = $1")
        .bind(&phone)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("delete old OTP", e))?;
    sqlx::query("INSERT INTO otp_codes (phone, code_hash, expires_at) VALUES ($1, $2, $3)")
        .bind(&phone)
        .bind(&code_hash)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("store OTP", e))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit OTP request", e))?;

    sender
        .send_sms(&phone, &format!("Your BD Marketplace login code is {code}"))
        .await
        .map_err(|e| ApiError::service_unavailable(format!("SMS delivery failed: {e}")))?;

    let dev_otp = if state.sms_dev_mode {
        Some(code)
    } else {
        None
    };
    Ok((phone, dev_otp))
}

/// Verify an OTP: check attempts, expiry and hash; on success create the
/// user if new (role `buyer`) and return the user row. Codes are
/// single-use — a successful verify deletes them.
///
/// Wave 39 (security audit CR-01 in the audit's full list — the
/// OTP attempt-cap race): the attempt count used to be a
/// read-check-write sequence (SELECT the row, compare attempts,
/// UPDATE on mismatch), so parallel verify calls could all read
/// `attempts = 4`, all pass the cap check, and brute-force past
/// the 5-attempt limit. The count is now ONE conditional UPDATE
/// that increments and returns the row only while the cap has
/// not been reached — the increment itself is the lock, so
/// concurrent verifies can never exceed the cap. Every verify
/// attempt counts (a successful one consumes the code anyway, so
/// the outcome is identical); error shapes are unchanged: no
/// code 400, cap reached 400, expired 400, wrong code 401.
pub async fn verify_otp_code(
    state: &AppState,
    raw_phone: &str,
    code: &str,
) -> Result<User, ApiError> {
    let pool = state.require_pool()?;
    let phone = normalize_bd_phone(raw_phone)
        .ok_or_else(|| ApiError::bad_request("invalid Bangladeshi phone number"))?;

    let row: Option<OtpRow> = sqlx::query_as(
        "UPDATE otp_codes SET attempts = attempts + 1 \
         WHERE phone = $1 AND attempts < $2 \
         RETURNING code_hash, expires_at",
    )
    .bind(&phone)
    .bind(OTP_MAX_ATTEMPTS)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("count OTP attempt", e))?;
    let row = match row {
        Some(row) => row,
        None => {
            // The conditional update matched nothing: either no
            // code was ever requested for this phone, or the
            // attempt cap is already reached. One existence read
            // tells the two apart so each keeps its own error.
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM otp_codes WHERE phone = $1)")
                    .bind(&phone)
                    .fetch_one(pool)
                    .await
                    .map_err(|e| ApiError::internal("check OTP existence", e))?;
            if exists {
                return Err(ApiError::bad_request(
                    "too many OTP attempts — request a new code",
                ));
            }
            return Err(ApiError::bad_request(
                "no OTP was requested for this phone",
            ));
        }
    };

    if row.expires_at < Utc::now() {
        return Err(ApiError::bad_request("OTP has expired — request a new code"));
    }
    if hash_otp(&phone, code) != row.code_hash {
        return Err(ApiError::unauthorized("incorrect OTP code"));
    }

    sqlx::query("DELETE FROM otp_codes WHERE phone = $1")
        .bind(&phone)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("consume OTP", e))?;

    find_or_create_user(pool, &phone).await
}

/// Find the user for a phone, creating a `buyer` account on first login.
pub async fn find_or_create_user(pool: &PgPool, phone: &str) -> Result<User, ApiError> {
    sqlx::query_as(&format!(
        "INSERT INTO users (phone) VALUES ($1) \
         ON CONFLICT (phone) DO UPDATE SET phone = EXCLUDED.phone \
         RETURNING {USER_COLUMNS}"
    ))
    .bind(phone)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("find or create user", e))
}

// ---------------------------------------------------------------------------
// Handlers (thin: parse -> call one service function -> respond)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RequestOtpRequest {
    pub phone: String,
}

#[derive(Serialize)]
pub struct RequestOtpResponse {
    pub phone: String,
    /// Present ONLY in dev mode (`APP_ENV=dev` / `SMS_DEV_MODE=true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev_otp: Option<String>,
}

pub async fn request_otp(
    State(state): State<AppState>,
    Json(req): Json<RequestOtpRequest>,
) -> Result<Json<RequestOtpResponse>, ApiError> {
    let (phone, dev_otp) = request_otp_code(&state, &req.phone).await?;
    Ok(Json(RequestOtpResponse { phone, dev_otp }))
}

#[derive(Deserialize)]
pub struct VerifyOtpRequest {
    pub phone: String,
    pub code: String,
}

#[derive(Serialize)]
pub struct VerifyOtpResponse {
    pub token: String,
    pub user: User,
}

pub async fn verify_otp(
    State(state): State<AppState>,
    Json(req): Json<VerifyOtpRequest>,
) -> Result<Json<VerifyOtpResponse>, ApiError> {
    let user = verify_otp_code(&state, &req.phone, &req.code).await?;
    let token = issue_token(&state, &user)?;
    Ok(Json(VerifyOtpResponse { token, user }))
}

/// The signed-in user's own row, re-read from the database: the
/// web app uses it to refresh what login stored (above all the
/// `is_admin` flag, which may have changed since the token was
/// issued).
pub async fn me(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<User>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(load_user(pool, user.user_id).await?))
}
