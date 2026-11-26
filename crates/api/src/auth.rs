//! Phone + OTP + JWT auth.
//!
//! Individually callable pieces, composed by the two thin handlers:
//! - `generate_otp` / `hash_otp` — pure OTP helpers (only the HASH of a
//!   code is ever stored, in `otp_codes`).
//! - `SmsSender` trait — how a code reaches the phone. The only
//!   implementation in Wave 2 is `LogSmsSender` (dev mode). In production
//!   mode with no real gateway configured, requesting an OTP returns a
//!   typed error — an SMS is never silently "sent" to nowhere.
//! - `issue_token` / `authenticate` — JWT (HS256) issue and verification.
//! - `request_otp_code` / `verify_otp_code` — the service functions that
//!   talk to the database; the handlers below only call them.

use axum::{
    extract::{FromRequestParts, State},
    http::{header::AUTHORIZATION, request::Parts, HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, TimeDelta, Utc};
use domain::normalize_bd_phone;
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

/// How an OTP message reaches a phone. One method, individually callable;
/// a real Bangladeshi SMS gateway joins later as another implementation.
pub trait SmsSender {
    fn send_sms(&self, phone: &str, message: &str) -> Result<(), ApiError>;
}

/// Dev-mode sender: logs the message (including the code) to stderr.
/// Only ever constructed when dev mode is explicitly enabled — see
/// `request_otp_code`.
pub struct LogSmsSender;

impl SmsSender for LogSmsSender {
    fn send_sms(&self, phone: &str, message: &str) -> Result<(), ApiError> {
        eprintln!("[dev SMS] to {phone}: {message}");
        Ok(())
    }
}

/// Pick the SMS sender for this state, or fail honestly: outside dev mode
/// no real gateway is configured in Wave 2, so OTP delivery is a typed
/// 503 — we never pretend an SMS was sent.
fn sms_sender(state: &AppState) -> Result<LogSmsSender, ApiError> {
    if state.sms_dev_mode {
        Ok(LogSmsSender)
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
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(state.jwt_secret.as_bytes()),
    )
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to sign token: {e}"),
        )
    })
}

/// The authenticated caller, extracted from a Bearer JWT.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub phone: String,
    pub role: String,
}

/// Verify the `Authorization: Bearer <jwt>` header. Single function used
/// by both the strict extractor (protected endpoints) and the optional
/// variant (public endpoints that behave differently for owners).
pub fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<AuthUser, ApiError> {
    let value = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("missing Authorization header"))?;
    let token = value
        .strip_prefix("Bearer ")
        .ok_or_else(|| ApiError::unauthorized("Authorization header must be a Bearer token"))?;
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(state.jwt_secret.as_bytes()),
        &Validation::new(Algorithm::HS256),
    )
    .map_err(|_| ApiError::unauthorized("invalid or expired token"))?;
    let user_id = Uuid::parse_str(&data.claims.sub)
        .map_err(|_| ApiError::unauthorized("token subject is not a user id"))?;
    Ok(AuthUser {
        user_id,
        phone: data.claims.phone,
        role: data.claims.role,
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
    attempts: i32,
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

    let code = generate_otp();
    let code_hash = hash_otp(&phone, &code);
    let expires_at = Utc::now() + TimeDelta::minutes(OTP_EXPIRY_MINUTES);

    // One active code per phone: replace any previous rows.
    sqlx::query("DELETE FROM otp_codes WHERE phone = $1")
        .bind(&phone)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("delete old OTP", e))?;
    sqlx::query("INSERT INTO otp_codes (phone, code_hash, expires_at) VALUES ($1, $2, $3)")
        .bind(&phone)
        .bind(&code_hash)
        .bind(expires_at)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("store OTP", e))?;

    sender.send_sms(&phone, &format!("Your BD Marketplace login code is {code}"))?;

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
pub async fn verify_otp_code(
    state: &AppState,
    raw_phone: &str,
    code: &str,
) -> Result<User, ApiError> {
    let pool = state.require_pool()?;
    let phone = normalize_bd_phone(raw_phone)
        .ok_or_else(|| ApiError::bad_request("invalid Bangladeshi phone number"))?;

    let row: Option<OtpRow> = sqlx::query_as(
        "SELECT code_hash, expires_at, attempts FROM otp_codes \
         WHERE phone = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&phone)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load OTP", e))?;
    let row = row.ok_or_else(|| ApiError::bad_request("no OTP was requested for this phone"))?;

    if row.attempts >= OTP_MAX_ATTEMPTS {
        return Err(ApiError::bad_request(
            "too many OTP attempts — request a new code",
        ));
    }
    if row.expires_at < Utc::now() {
        return Err(ApiError::bad_request("OTP has expired — request a new code"));
    }
    if hash_otp(&phone, code) != row.code_hash {
        sqlx::query("UPDATE otp_codes SET attempts = attempts + 1 WHERE phone = $1")
            .bind(&phone)
            .execute(pool)
            .await
            .map_err(|e| ApiError::internal("count OTP attempt", e))?;
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
