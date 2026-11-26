//! Product Q&A (Wave 25).
//!
//! The rules, each enforced by exactly one function below:
//! - Asking: any signed-in user may ask on a PUBLICLY VISIBLE
//!   product (the shared `products::get_public_product` rule —
//!   anything else is a 404), except on a product of their OWN
//!   store, which is a 400: sellers answer questions, they do
//!   not seed them.
//! - Answering: only the owner of the product's store may answer
//!   (`require_question_store` — everyone else gets a 404, so a
//!   question's existence is not leaked). Re-answering UPDATES
//!   the same row: an answer is a correction away from being
//!   right, never a second answer stacked on the first.
//! - Moderation: the store owner and platform admins share ONE
//!   function, `set_question_hidden`. A hidden question leaves
//!   the public list and the dashboard's unanswered count, but
//!   stays in the seller inbox (marked hidden) so the decision
//!   can be undone.
//! - Identity: public views show the asker only as their masked
//!   phone (`domain::mask_phone`) — never the full number, never
//!   the user id.
//!
//! Input rules (question 3..=500 chars, answer 1..=1000 chars)
//! are the pure domain validators.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{mask_phone, validate_answer_body, validate_question_body};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::admin::require_admin;
use crate::audit::record_admin_action;
use crate::auth::AuthUser;
use crate::products::{clamp_pagination, get_public_product};
use crate::stores::get_store_by_id;
use crate::{ApiError, AppState};

/// The public question shape: the row plus its computed asker
/// display string (masked phone) and the answered flag.
#[derive(Debug, Serialize)]
pub struct ProductQuestion {
    pub id: Uuid,
    pub product_id: Uuid,
    pub body: String,
    /// The asker's masked phone — never the full number.
    pub asker_display: String,
    pub answer_body: Option<String>,
    pub answered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub is_answered: bool,
}

/// The product reference carried by seller-inbox items.
#[derive(Debug, Serialize)]
pub struct QuestionProduct {
    pub id: Uuid,
    pub title: String,
    pub store_slug: String,
}

/// One question as the seller inbox (and the moderation
/// endpoints) sees it: the public view plus the product it
/// belongs to and the hidden flag the public never sees.
#[derive(Debug, Serialize)]
pub struct SellerQuestion {
    #[serde(flatten)]
    pub question: ProductQuestion,
    pub product: QuestionProduct,
    pub is_hidden: bool,
}

/// One page of public questions plus the total count — the same
/// shape as the review list.
#[derive(Debug, Serialize)]
pub struct QuestionList {
    pub items: Vec<ProductQuestion>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// One page of seller-inbox questions plus the total count.
#[derive(Debug, Serialize)]
pub struct SellerQuestionList {
    pub items: Vec<SellerQuestion>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// Flat row for the public question SELECT: the question columns
/// plus the asker's phone (masked on the way out).
#[derive(Debug, FromRow)]
struct QuestionRow {
    id: Uuid,
    product_id: Uuid,
    body: String,
    asker_phone: String,
    answer_body: Option<String>,
    answered_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

impl QuestionRow {
    fn into_view(self) -> ProductQuestion {
        ProductQuestion {
            id: self.id,
            product_id: self.product_id,
            body: self.body,
            asker_display: mask_phone(&self.asker_phone),
            is_answered: self.answer_body.is_some(),
            answer_body: self.answer_body,
            answered_at: self.answered_at,
            created_at: self.created_at,
        }
    }
}

/// Flat row for the seller-inbox SELECT: the question columns,
/// the asker's phone, the hidden flag, and the product's title
/// and store slug.
#[derive(Debug, FromRow)]
struct SellerQuestionRow {
    id: Uuid,
    product_id: Uuid,
    body: String,
    asker_phone: String,
    answer_body: Option<String>,
    answered_at: Option<DateTime<Utc>>,
    is_hidden: bool,
    created_at: DateTime<Utc>,
    product_title: String,
    store_slug: String,
}

impl SellerQuestionRow {
    fn into_view(self) -> SellerQuestion {
        SellerQuestion {
            question: ProductQuestion {
                id: self.id,
                product_id: self.product_id,
                body: self.body,
                asker_display: mask_phone(&self.asker_phone),
                is_answered: self.answer_body.is_some(),
                answer_body: self.answer_body,
                answered_at: self.answered_at,
                created_at: self.created_at,
            },
            product: QuestionProduct {
                id: self.product_id,
                title: self.product_title,
                store_slug: self.store_slug,
            },
            is_hidden: self.is_hidden,
        }
    }
}

/// Question columns for the public SELECT; every public question
/// query selects this from `QUESTION_FROM` so all views are built
/// identically.
const QUESTION_COLUMNS: &str = "q.id, q.product_id, q.body, \
    u.phone AS asker_phone, q.answer_body, q.answered_at, q.created_at";

const QUESTION_FROM: &str = "FROM product_questions q \
    JOIN users u ON u.id = q.asker_user_id";

/// Question + product columns for the seller-inbox SELECT.
const SELLER_QUESTION_COLUMNS: &str = "q.id, q.product_id, q.body, \
    u.phone AS asker_phone, q.answer_body, q.answered_at, q.is_hidden, \
    q.created_at, p.title AS product_title, s.slug AS store_slug";

const SELLER_QUESTION_FROM: &str = "FROM product_questions q \
    JOIN users u ON u.id = q.asker_user_id \
    JOIN products p ON p.id = q.product_id \
    JOIN stores s ON s.id = p.store_id";

// ---------------------------------------------------------------------------
// Service functions
// ---------------------------------------------------------------------------

/// Load one question in public view shape (hidden or not — the
/// public LIST filters hidden out; single loads serve the
/// write paths, which return the row they just changed).
pub async fn get_question_by_id(pool: &PgPool, id: Uuid) -> Result<ProductQuestion, ApiError> {
    let row: QuestionRow = sqlx::query_as(&format!(
        "SELECT {QUESTION_COLUMNS} {QUESTION_FROM} WHERE q.id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load question", e))?
    .ok_or_else(|| ApiError::not_found("question not found"))?;
    Ok(row.into_view())
}

/// Load one question in seller-inbox shape (with product and
/// hidden flag) — the moderation endpoints return this so the
/// caller sees the state they just set.
pub async fn get_seller_question_by_id(pool: &PgPool, id: Uuid) -> Result<SellerQuestion, ApiError> {
    let row: SellerQuestionRow = sqlx::query_as(&format!(
        "SELECT {SELLER_QUESTION_COLUMNS} {SELLER_QUESTION_FROM} WHERE q.id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load seller question", e))?
    .ok_or_else(|| ApiError::not_found("question not found"))?;
    Ok(row.into_view())
}

/// Ask a question on a product. Order of checks: input (400),
/// product publicly visible (404), not the caller's own store
/// (400).
pub async fn ask_question(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    body: &str,
) -> Result<ProductQuestion, ApiError> {
    let body = validate_question_body(body).map_err(ApiError::bad_request)?;
    let product = get_public_product(pool, product_id).await?;
    let store = get_store_by_id(pool, product.store_id).await?;
    if store.owner_user_id == user.user_id {
        return Err(ApiError::bad_request(
            "you cannot ask a question about your own product",
        ));
    }

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO product_questions (product_id, asker_user_id, body) \
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(product_id)
    .bind(user.user_id)
    .bind(body)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("create question", e))?;

    get_question_by_id(pool, id).await
}

/// List a product's questions for the public, newest first,
/// paginated like every other list in the API. Hidden questions
/// are excluded; a product that is not publicly visible is a
/// 404, not an empty list.
pub async fn list_product_questions(
    pool: &PgPool,
    product_id: Uuid,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<QuestionList, ApiError> {
    get_public_product(pool, product_id).await?;
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM product_questions \
         WHERE product_id = $1 AND NOT is_hidden",
    )
    .bind(product_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count questions", e))?;

    let rows: Vec<QuestionRow> = sqlx::query_as(&format!(
        "SELECT {QUESTION_COLUMNS} {QUESTION_FROM} \
         WHERE q.product_id = $1 AND NOT q.is_hidden \
         ORDER BY q.created_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(product_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list questions", e))?;

    Ok(QuestionList {
        items: rows.into_iter().map(QuestionRow::into_view).collect(),
        total,
        page,
        limit,
    })
}

/// Resolve a question to the store that sells its product,
/// requiring the caller to belong to that store's team with
/// the `answer_questions` right (Wave 34: owner, manager or
/// staff). Every failure is a 404: a question that is not
/// the caller's to moderate is indistinguishable from one
/// that does not exist.
async fn require_question_store(
    pool: &PgPool,
    user: &AuthUser,
    question_id: Uuid,
) -> Result<Uuid, ApiError> {
    let store_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT p.store_id FROM product_questions q \
         JOIN products p ON p.id = q.product_id WHERE q.id = $1",
    )
    .bind(question_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load question store", e))?;
    let store_id = store_id.ok_or_else(|| ApiError::not_found("question not found"))?;
    // Wave 34: the owner, or a member whose role carries
    // `answer_questions` (every member role does). No
    // relationship keeps this module's 404.
    match crate::access::store_role(pool, user.user_id, store_id).await? {
        Some(role) => {
            crate::access::require_role(&role, domain::STORE_ACTION_ANSWER_QUESTIONS)?;
        }
        None => return Err(ApiError::not_found("question not found")),
    }
    Ok(store_id)
}

/// Answer a question as the owner of the product's store.
/// Answering an already-answered question UPDATES the answer —
/// the seller correcting themselves is a feature, not a
/// conflict.
pub async fn answer_question(
    pool: &PgPool,
    user: &AuthUser,
    question_id: Uuid,
    body: &str,
) -> Result<ProductQuestion, ApiError> {
    let body = validate_answer_body(body).map_err(ApiError::bad_request)?;
    require_question_store(pool, user, question_id).await?;

    sqlx::query(
        "UPDATE product_questions \
         SET answer_body = $2, answered_by_user_id = $3, answered_at = now() \
         WHERE id = $1",
    )
    .bind(question_id)
    .bind(body)
    .bind(user.user_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("answer question", e))?;

    get_question_by_id(pool, question_id).await
}

/// THE moderation write: set a question's hidden flag. Seller
/// and admin endpoints both land here after their own guard
/// (ownership / admin) — this function only performs the state
/// change and answers with the resulting seller-shape view. A
/// missing question is a 404.
pub async fn set_question_hidden(
    pool: &PgPool,
    question_id: Uuid,
    hidden: bool,
) -> Result<SellerQuestion, ApiError> {
    let affected = sqlx::query("UPDATE product_questions SET is_hidden = $2 WHERE id = $1")
        .bind(question_id)
        .bind(hidden)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("set question hidden", e))?
        .rows_affected();
    if affected == 0 {
        return Err(ApiError::not_found("question not found"));
    }
    get_seller_question_by_id(pool, question_id).await
}

/// The ADMIN moderation write (Wave 28): the same flag flip
/// as [`set_question_hidden`], but performed by a platform
/// admin, so the flip and its audit row (`qna.hide` /
/// `qna.unhide`) commit in one transaction. A missing
/// question is a 404 and writes no audit row.
pub async fn admin_set_question_hidden(
    pool: &PgPool,
    admin_id: Uuid,
    question_id: Uuid,
    hidden: bool,
) -> Result<SellerQuestion, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin admin set question hidden", e))?;
    let affected = sqlx::query("UPDATE product_questions SET is_hidden = $2 WHERE id = $1")
        .bind(question_id)
        .bind(hidden)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("admin set question hidden", e))?
        .rows_affected();
    if affected == 0 {
        return Err(ApiError::not_found("question not found"));
    }
    record_admin_action(
        &mut *tx,
        admin_id,
        if hidden { "qna.hide" } else { "qna.unhide" },
        "question",
        Some(&question_id.to_string()),
        None,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit admin set question hidden", e))?;
    get_seller_question_by_id(pool, question_id).await
}

/// The seller inbox: questions across every store the caller
/// may answer for (Wave 34: own stores plus member stores),
/// newest first. `answered` selects the queue — `false`
/// (the default) is the work queue, `true` the answered ones,
/// `all` everything; hidden questions are included and marked,
/// because the inbox is where a hide is undone.
pub async fn list_seller_questions(
    pool: &PgPool,
    user: &AuthUser,
    answered: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<SellerQuestionList, ApiError> {
    let answered_filter: Option<bool> = match answered {
        None | Some("false") => Some(false),
        Some("true") => Some(true),
        Some("all") => None,
        Some(_) => {
            return Err(ApiError::bad_request(
                "answered must be false, true or all",
            ))
        }
    };
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    // Wave 34: the inbox spans every store the caller may
    // answer questions in (own stores + member stores).
    let store_ids =
        crate::access::accessible_store_ids(pool, user.user_id, domain::STORE_ACTION_ANSWER_QUESTIONS)
            .await?;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM product_questions q \
         JOIN products p ON p.id = q.product_id \
         WHERE p.store_id = ANY($1) \
           AND ($2::boolean IS NULL OR (q.answer_body IS NOT NULL) = $2)",
    )
    .bind(&store_ids)
    .bind(answered_filter)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count seller questions", e))?;

    let rows: Vec<SellerQuestionRow> = sqlx::query_as(&format!(
        "SELECT {SELLER_QUESTION_COLUMNS} {SELLER_QUESTION_FROM} \
         WHERE p.store_id = ANY($1) \
           AND ($2::boolean IS NULL OR (q.answer_body IS NOT NULL) = $2) \
         ORDER BY q.created_at DESC LIMIT $3 OFFSET $4"
    ))
    .bind(&store_ids)
    .bind(answered_filter)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list seller questions", e))?;

    Ok(SellerQuestionList {
        items: rows.into_iter().map(SellerQuestionRow::into_view).collect(),
        total,
        page,
        limit,
    })
}

/// How many questions across a store scope still need an
/// answer — the dashboard's Q&A counter (the caller passes
/// the scope: the owner's stores for the default dashboard,
/// one store for the Wave 34 store view). Hidden questions
/// are EXCLUDED: a question the seller (or an admin) already
/// dealt with by hiding it must not keep nagging as
/// "unanswered".
pub async fn count_unanswered_questions(
    pool: &PgPool,
    store_ids: &[Uuid],
) -> Result<i64, ApiError> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM product_questions q \
         JOIN products p ON p.id = q.product_id \
         WHERE p.store_id = ANY($1) AND q.answer_body IS NULL AND NOT q.is_hidden",
    )
    .bind(store_ids)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count unanswered questions", e))
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AskQuestionInput {
    pub body: String,
}

#[derive(Deserialize)]
pub struct AnswerQuestionInput {
    pub body: String,
}

#[derive(Deserialize)]
pub struct ListQuestionsQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct ListSellerQuestionsQuery {
    pub answered: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn ask_question_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(input): Json<AskQuestionInput>,
) -> Result<(StatusCode, Json<ProductQuestion>), ApiError> {
    let pool = state.require_pool()?;
    let question = ask_question(pool, &user, product_id, &input.body).await?;
    Ok((StatusCode::CREATED, Json(question)))
}

pub async fn list_questions_handler(
    State(state): State<AppState>,
    Path(product_id): Path<Uuid>,
    Query(query): Query<ListQuestionsQuery>,
) -> Result<Json<QuestionList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_product_questions(pool, product_id, query.page, query.limit).await?,
    ))
}

pub async fn answer_question_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<AnswerQuestionInput>,
) -> Result<Json<ProductQuestion>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(answer_question(pool, &user, id, &input.body).await?))
}

pub async fn hide_question_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<SellerQuestion>, ApiError> {
    let pool = state.require_pool()?;
    require_question_store(pool, &user, id).await?;
    Ok(Json(set_question_hidden(pool, id, true).await?))
}

pub async fn unhide_question_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<SellerQuestion>, ApiError> {
    let pool = state.require_pool()?;
    require_question_store(pool, &user, id).await?;
    Ok(Json(set_question_hidden(pool, id, false).await?))
}

pub async fn admin_hide_question_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<SellerQuestion>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        admin_set_question_hidden(pool, admin.id, id, true).await?,
    ))
}

pub async fn admin_unhide_question_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<SellerQuestion>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        admin_set_question_hidden(pool, admin.id, id, false).await?,
    ))
}

pub async fn list_seller_questions_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ListSellerQuestionsQuery>,
) -> Result<Json<SellerQuestionList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_seller_questions(
            pool,
            &user,
            query.answered.as_deref(),
            query.page,
            query.limit,
        )
        .await?,
    ))
}
