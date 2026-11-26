//! Wave 13: buyer <-> store messaging + the SIDRA bridge.
//!
//! The inbox: a buyer opens a conversation with a store
//! (optionally about one product), and the two sides exchange
//! messages. One OPEN conversation per (store, buyer, product)
//! triple — enforced by the find-or-create service function with
//! a lookup-then-insert, because the nullable `product_id` keeps
//! the database from expressing the rule (see migration 0015).
//! Read state lives on the conversation as two markers
//! (`buyer_last_read_at` / `seller_last_read_at`): opening the
//! thread stamps the caller's marker, and a side's unread count
//! is the other side's messages newer than its marker. AI
//! messages (sender_type `ai`) count as unread for the buyer;
//! they never make the seller's side unread — the seller either
//! approved them or the store opted into auto-reply.
//!
//! The SIDRA bridge: SIDRA is the platform's external AI
//! salesperson service. It is a separate project and is NEVER
//! reimplemented here — this module is only the platform side of
//! the contract:
//!
//! Outbound (best effort): when a buyer posts a message and the
//! bridge is configured, the API fires a signed POST to
//! `SIDRA_API_URL` in a detached task. A failure is logged and
//! never fails the buyer's request — the notification is a hint,
//! and the signed callback below is the reliable path.
//!
//! Inbound (the reliable path): SIDRA POSTs its reply to
//! `/v1/sidra/reply` with an HMAC-SHA256 signature (hex) over the
//! domain's canonical payload
//! (`domain::sidra_signature_payload`). The verdict decides
//! everything: a bad signature is a 401 and nothing is stored.
//! A valid reply lands one of two ways, by the store's
//! `sidra_auto_reply` flag: directly as an `ai` message, or as a
//! pending row in `sidra_suggestions` that the seller sends or
//! dismisses. Every callback outcome — accepted or rejected, and
//! why — is logged to stderr. Unlike payments there is no audit
//! table: no money moves on a chat reply, so a stored row exists
//! only when a reply was actually accepted (the message or the
//! suggestion itself is the record).
//!
//! Honest degradation: with no SIDRA configuration (both
//! `SIDRA_API_URL` and `SIDRA_WEBHOOK_SECRET` are needed), the
//! callback endpoint answers 503, no notification is attempted,
//! and the inbox works completely as a human chat.

use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{mask_phone, sidra_signature_payload, validate_message_body};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::products::get_product_by_id;
use crate::stores::{get_store_by_id, get_store_by_owner, Store};
use crate::{ApiError, AppState, SidraConfig};

type HmacSha256 = Hmac<Sha256>;

/// How many messages one thread read returns (the newest window).
pub const THREAD_MESSAGE_LIMIT: i64 = 200;

/// Characters of the newest message shown as a list preview.
const PREVIEW_CHARS: usize = 80;

// ---------------------------------------------------------------------------
// SIDRA bridge signatures — the one implementation, shared by the
// outbound notifier (signer) and the callback endpoint (verifier),
// over the domain's canonical payload.
// ---------------------------------------------------------------------------

/// Sign a SIDRA bridge payload: HMAC-SHA256 (hex) over
/// [`sidra_signature_payload`]. Used for the outbound notify
/// header; SIDRA signs its callbacks with the same function
/// shape on its side.
pub fn sign_sidra_callback(
    secret: &str,
    conversation_id: &str,
    message_id: &str,
    text: &str,
) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(sidra_signature_payload(conversation_id, message_id, text).as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Verify a SIDRA callback signature produced by
/// [`sign_sidra_callback`]. Malformed hex or a wrong signature
/// both return false; the MAC comparison itself is constant-time.
pub fn verify_sidra_callback(
    secret: &str,
    conversation_id: &str,
    message_id: &str,
    text: &str,
    signature_hex: &str,
) -> bool {
    let Ok(signature) = hex::decode(signature_hex) else {
        return false;
    };
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(sidra_signature_payload(conversation_id, message_id, text).as_bytes());
    mac.verify_slice(&signature).is_ok()
}

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// One message in a conversation (a row of `messages`).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Message {
    pub id: Uuid,
    pub conversation_id: Uuid,
    /// buyer | seller | ai
    pub sender_type: String,
    /// The user who sent it; null for AI messages.
    pub sender_id: Option<Uuid>,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

/// The store half of a conversation view.
#[derive(Debug, Clone, Serialize)]
pub struct ConversationStore {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

/// One conversation as the caller sees it: their `role` in it
/// (buyer or seller), the unread count FOR THEIR SIDE, and a
/// preview of the newest message.
#[derive(Debug, Clone, Serialize)]
pub struct ConversationView {
    pub id: Uuid,
    pub store: ConversationStore,
    pub buyer_id: Uuid,
    /// Buyer display for the seller side: name when set, else the
    /// masked phone (the full number is never exposed).
    pub buyer_display: String,
    pub product_id: Option<Uuid>,
    pub product_title: Option<String>,
    /// buyer | seller — the caller's role in this conversation.
    pub role: String,
    /// open | closed
    pub status: String,
    pub last_message_at: DateTime<Utc>,
    /// First 80 characters of the newest message; null while the
    /// conversation is empty.
    pub last_message_preview: Option<String>,
    /// Messages from the other side the caller has not read yet.
    pub unread_count: i64,
    pub created_at: DateTime<Utc>,
}

/// A conversation thread: the view plus its messages, oldest
/// first (the newest [`THREAD_MESSAGE_LIMIT`] when longer).
#[derive(Debug, Serialize)]
pub struct ConversationThread {
    pub conversation: ConversationView,
    pub messages: Vec<Message>,
}

/// One pending-or-decided SIDRA reply suggestion.
#[derive(Debug, Clone, Serialize)]
pub struct SidraSuggestion {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub in_reply_to_message_id: Uuid,
    pub body: String,
    /// pending | sent | dismissed
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    /// Who SIDRA was answering (name or masked phone).
    pub buyer_display: String,
}

/// The seller-facing SIDRA status: whether the platform bridge
/// is configured at all, and this store's auto-reply mode.
#[derive(Debug, Serialize)]
pub struct SidraStatus {
    pub configured: bool,
    pub auto_reply: bool,
}

/// The callback endpoint's answer: how the reply landed.
#[derive(Debug, Serialize)]
pub struct SidraReplyResponse {
    /// sent | suggested
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Conversation rows + views
// ---------------------------------------------------------------------------

/// A conversation row joined with everything its view needs,
/// including both sides' unread counts (the view picks the
/// caller's). One query shape serves the single load, the
/// find-or-create reload and the caller's list.
#[derive(Debug, FromRow)]
struct ConversationRow {
    id: Uuid,
    store_id: Uuid,
    store_name: String,
    store_slug: String,
    store_owner_id: Uuid,
    buyer_id: Uuid,
    buyer_name: Option<String>,
    buyer_phone: String,
    product_id: Option<Uuid>,
    product_title: Option<String>,
    status: String,
    last_message_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    last_message_body: Option<String>,
    unread_for_buyer: i64,
    unread_for_seller: i64,
}

const CONVERSATION_SELECT: &str = "SELECT c.id, c.store_id, \
    s.name AS store_name, s.slug AS store_slug, \
    s.owner_user_id AS store_owner_id, \
    c.buyer_id, u.name AS buyer_name, u.phone AS buyer_phone, \
    c.product_id, p.title AS product_title, \
    c.status, c.last_message_at, c.created_at, \
    (SELECT m.body FROM messages m WHERE m.conversation_id = c.id \
       ORDER BY m.created_at DESC, m.id DESC LIMIT 1) AS last_message_body, \
    (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id \
       AND m.sender_type IN ('seller', 'ai') \
       AND m.created_at > COALESCE(c.buyer_last_read_at, 'epoch'::timestamptz))::bigint \
      AS unread_for_buyer, \
    (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id \
       AND m.sender_type = 'buyer' \
       AND m.created_at > COALESCE(c.seller_last_read_at, 'epoch'::timestamptz))::bigint \
      AS unread_for_seller \
    FROM conversations c \
    JOIN stores s ON s.id = c.store_id \
    JOIN users u ON u.id = c.buyer_id \
    LEFT JOIN products p ON p.id = c.product_id";

/// Display name for a buyer: their name when set, otherwise their
/// masked phone — never the full number (same rule as reviews).
fn buyer_display(name: &Option<String>, phone: &str) -> String {
    match name {
        Some(name) if !name.trim().is_empty() => name.clone(),
        _ => mask_phone(phone),
    }
}

impl ConversationRow {
    /// The caller's role in this conversation: the buyer when the
    /// caller is the buyer (a store owner asking about their own
    /// purchase is still the buyer here), else the seller side.
    fn role_for(&self, caller_id: Uuid) -> &'static str {
        if self.buyer_id == caller_id {
            "buyer"
        } else {
            "seller"
        }
    }

    fn view_for(&self, caller_id: Uuid) -> ConversationView {
        let role = self.role_for(caller_id);
        ConversationView {
            id: self.id,
            store: ConversationStore {
                id: self.store_id,
                name: self.store_name.clone(),
                slug: self.store_slug.clone(),
            },
            buyer_id: self.buyer_id,
            buyer_display: buyer_display(&self.buyer_name, &self.buyer_phone),
            product_id: self.product_id,
            product_title: self.product_title.clone(),
            role: role.to_string(),
            status: self.status.clone(),
            last_message_at: self.last_message_at,
            last_message_preview: self
                .last_message_body
                .as_ref()
                .map(|body| body.chars().take(PREVIEW_CHARS).collect()),
            unread_count: if role == "buyer" {
                self.unread_for_buyer
            } else {
                self.unread_for_seller
            },
            created_at: self.created_at,
        }
    }
}

/// Load one conversation row by id (404 when there is none).
async fn load_conversation_row(
    pool: &PgPool,
    conversation_id: Uuid,
) -> Result<ConversationRow, ApiError> {
    sqlx::query_as(&format!("{CONVERSATION_SELECT} WHERE c.id = $1"))
        .bind(conversation_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load conversation", e))?
        .ok_or_else(|| ApiError::not_found("conversation not found"))
}

/// Load a conversation the caller takes part in, returning the
/// row and the caller's role. Anyone else gets a 404 — a stranger
/// cannot even learn the conversation exists (the platform's
/// no-leak rule for private data).
async fn load_conversation_for_participant(
    pool: &PgPool,
    user: &AuthUser,
    conversation_id: Uuid,
) -> Result<(ConversationRow, &'static str), ApiError> {
    let row = load_conversation_row(pool, conversation_id).await?;
    if row.buyer_id != user.user_id && row.store_owner_id != user.user_id {
        return Err(ApiError::not_found("conversation not found"));
    }
    let role = row.role_for(user.user_id);
    Ok((row, role))
}

// ---------------------------------------------------------------------------
// Conversation service functions
// ---------------------------------------------------------------------------

/// Find the caller's OPEN conversation with a store (about a
/// product, when given), or create it. The store must exist and
/// be active — a suspended store is a 404, as everywhere public.
/// A given product must exist and belong to that store.
/// Returns the view and whether the conversation is new.
pub async fn find_or_create_conversation(
    pool: &PgPool,
    user: &AuthUser,
    store_id: Uuid,
    product_id: Option<Uuid>,
) -> Result<(ConversationView, bool), ApiError> {
    let store = get_store_by_id(pool, store_id).await?;
    if store.status != "active" {
        return Err(ApiError::not_found("store not found"));
    }
    if let Some(product_id) = product_id {
        let product = get_product_by_id(pool, product_id).await?;
        if product.store_id != store_id {
            return Err(ApiError::bad_request(
                "product does not belong to this store",
            ));
        }
    }

    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM conversations \
         WHERE store_id = $1 AND buyer_id = $2 \
           AND product_id IS NOT DISTINCT FROM $3 AND status = 'open' \
         ORDER BY created_at ASC LIMIT 1",
    )
    .bind(store_id)
    .bind(user.user_id)
    .bind(product_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("find conversation", e))?;
    if let Some(id) = existing {
        let row = load_conversation_row(pool, id).await?;
        return Ok((row.view_for(user.user_id), false));
    }

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO conversations (store_id, buyer_id, product_id) \
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(store_id)
    .bind(user.user_id)
    .bind(product_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("create conversation", e))?;
    let row = load_conversation_row(pool, id).await?;
    Ok((row.view_for(user.user_id), true))
}

/// Every conversation the caller takes part in — as a buyer with
/// stores, and as a seller with buyers — merged into one list,
/// newest activity first, each with the caller's role in it.
pub async fn list_conversations_for_user(
    pool: &PgPool,
    user: &AuthUser,
) -> Result<Vec<ConversationView>, ApiError> {
    let rows: Vec<ConversationRow> = sqlx::query_as(&format!(
        "{CONVERSATION_SELECT} \
         WHERE c.buyer_id = $1 OR s.owner_user_id = $1 \
         ORDER BY c.last_message_at DESC, c.id DESC"
    ))
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list conversations", e))?;
    Ok(rows.iter().map(|row| row.view_for(user.user_id)).collect())
}

/// Read a thread: stamp the caller's read marker (their unread
/// count for this conversation is zero from this moment), then
/// return the view plus the newest window of messages, oldest
/// first.
pub async fn read_conversation_thread(
    pool: &PgPool,
    user: &AuthUser,
    conversation_id: Uuid,
) -> Result<ConversationThread, ApiError> {
    let (row, role) = load_conversation_for_participant(pool, user, conversation_id).await?;
    let marker_column = if role == "buyer" {
        "buyer_last_read_at"
    } else {
        "seller_last_read_at"
    };
    sqlx::query(&format!(
        "UPDATE conversations SET {marker_column} = now() WHERE id = $1"
    ))
    .bind(conversation_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("mark conversation read", e))?;

    let messages: Vec<Message> = sqlx::query_as(
        "SELECT id, conversation_id, sender_type, sender_id, body, created_at \
         FROM (SELECT id, conversation_id, sender_type, sender_id, body, created_at \
               FROM messages WHERE conversation_id = $1 \
               ORDER BY created_at DESC, id DESC LIMIT $2) newest \
         ORDER BY created_at ASC, id ASC",
    )
    .bind(conversation_id)
    .bind(THREAD_MESSAGE_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load messages", e))?;

    let row = load_conversation_row(pool, row.id).await?;
    Ok(ConversationThread {
        conversation: row.view_for(user.user_id),
        messages,
    })
}

/// Insert one message and move the conversation: bump
/// `last_message_at` to the message's own timestamp and stamp
/// the sender side's read marker (a sender has by definition
/// read everything up to their own message). AI messages stamp
/// neither marker — the buyer still has to see the reply.
async fn insert_message(
    pool: &PgPool,
    conversation_id: Uuid,
    sender_type: &str,
    sender_id: Option<Uuid>,
    body: &str,
) -> Result<Message, ApiError> {
    let message: Message = sqlx::query_as(
        "INSERT INTO messages (conversation_id, sender_type, sender_id, body) \
         VALUES ($1, $2, $3, $4) \
         RETURNING id, conversation_id, sender_type, sender_id, body, created_at",
    )
    .bind(conversation_id)
    .bind(sender_type)
    .bind(sender_id)
    .bind(body)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("insert message", e))?;

    sqlx::query(
        "UPDATE conversations SET last_message_at = $2, \
           buyer_last_read_at = CASE WHEN $3 = 'buyer' THEN $2 ELSE buyer_last_read_at END, \
           seller_last_read_at = CASE WHEN $3 = 'seller' THEN $2 ELSE seller_last_read_at END \
         WHERE id = $1",
    )
    .bind(conversation_id)
    .bind(message.created_at)
    .bind(sender_type)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("bump conversation", e))?;

    Ok(message)
}

/// Post a message as a conversation participant. The sender type
/// comes from the caller's role, never from the request. A closed
/// conversation rejects new messages with a 409. When a buyer
/// writes and the SIDRA bridge is configured, SIDRA is notified
/// (best effort — see [`notify_sidra`]).
pub async fn post_message(
    state: &AppState,
    pool: &PgPool,
    user: &AuthUser,
    conversation_id: Uuid,
    body: &str,
) -> Result<Message, ApiError> {
    let (row, role) = load_conversation_for_participant(pool, user, conversation_id).await?;
    if row.status != "open" {
        return Err(ApiError::conflict("conversation is closed"));
    }
    validate_message_body(body).map_err(ApiError::bad_request)?;
    let message = insert_message(
        pool,
        conversation_id,
        role,
        Some(user.user_id),
        body.trim(),
    )
    .await?;
    if role == "buyer" {
        notify_sidra(&state.sidra_config, &row, &message);
    }
    Ok(message)
}

/// Close a conversation (either participant). Closing is
/// idempotent; a closed conversation keeps its history readable
/// but rejects new messages, and find-or-create starts a fresh
/// conversation for the same triple afterwards.
pub async fn close_conversation(
    pool: &PgPool,
    user: &AuthUser,
    conversation_id: Uuid,
) -> Result<ConversationView, ApiError> {
    let (row, _) = load_conversation_for_participant(pool, user, conversation_id).await?;
    sqlx::query("UPDATE conversations SET status = 'closed' WHERE id = $1")
        .bind(row.id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("close conversation", e))?;
    let row = load_conversation_row(pool, row.id).await?;
    Ok(row.view_for(user.user_id))
}

/// Fire-and-forget notification to SIDRA about a new buyer
/// message: a signed POST in a detached task. Best effort by
/// design — any failure (unreachable service, non-2xx answer) is
/// logged and dropped; the buyer's request has already succeeded
/// and the signed callback is the reliable direction.
fn notify_sidra(config: &SidraConfig, conversation: &ConversationRow, message: &Message) {
    let (Some(url), Some(secret)) = (config.api_url.clone(), config.webhook_secret.clone()) else {
        return;
    };
    let signature = sign_sidra_callback(
        &secret,
        &conversation.id.to_string(),
        &message.id.to_string(),
        &message.body,
    );
    let payload = serde_json::json!({
        "conversation_id": conversation.id,
        "store_id": conversation.store_id,
        "buyer_message_id": message.id,
        "body": message.body,
        "product_id": conversation.product_id,
    });
    let conversation_id = conversation.id;
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(client) => client,
            Err(err) => {
                eprintln!("sidra notify: could not build HTTP client: {err}");
                return;
            }
        };
        match client
            .post(&url)
            .header("X-Sidra-Signature", signature)
            .json(&payload)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                eprintln!(
                    "sidra notify for conversation {conversation_id} answered {}",
                    response.status()
                );
            }
            Err(err) => {
                eprintln!("sidra notify for conversation {conversation_id} failed: {err}");
            }
        }
    });
}

// ---------------------------------------------------------------------------
// SIDRA callback (inbound)
// ---------------------------------------------------------------------------

/// Apply one SIDRA reply callback. The order of checks is
/// deliberate and every outcome is logged:
///
/// An unconfigured bridge is a 503 before anything is read. An
/// invalid reply body is a 400. A bad signature is a 401 and
/// NOTHING is stored. An unknown conversation, or one that does
/// not contain the answered message, is a 404. A closed
/// conversation is a 409. Only then does the reply land — as an
/// `ai` message when the store enabled auto-reply, else as a
/// pending suggestion for the seller.
pub async fn apply_sidra_reply(
    state: &AppState,
    pool: &PgPool,
    conversation_id: Uuid,
    in_reply_to_message_id: Uuid,
    reply_text: &str,
    signature: &str,
) -> Result<SidraReplyResponse, ApiError> {
    let Some(secret) = state.sidra_config.webhook_secret.as_deref() else {
        eprintln!("sidra reply rejected: bridge is not configured");
        return Err(ApiError::service_unavailable(
            "SIDRA bridge is not configured",
        ));
    };
    validate_message_body(reply_text).map_err(ApiError::bad_request)?;
    let reply_text = reply_text.trim();

    if !verify_sidra_callback(
        secret,
        &conversation_id.to_string(),
        &in_reply_to_message_id.to_string(),
        reply_text,
        signature,
    ) {
        eprintln!(
            "sidra reply rejected: invalid signature (conversation {conversation_id})"
        );
        return Err(ApiError::unauthorized("invalid SIDRA signature"));
    }

    let row = match load_conversation_row(pool, conversation_id).await {
        Ok(row) => row,
        Err(err) => {
            eprintln!(
                "sidra reply rejected: conversation {conversation_id} not loadable ({})",
                err.message
            );
            return Err(err);
        }
    };
    if row.status != "open" {
        eprintln!(
            "sidra reply rejected: conversation {conversation_id} is closed"
        );
        return Err(ApiError::conflict("conversation is closed"));
    }
    let answered_in: Option<Uuid> = sqlx::query_scalar(
        "SELECT conversation_id FROM messages WHERE id = $1",
    )
    .bind(in_reply_to_message_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load answered message", e))?;
    if answered_in != Some(conversation_id) {
        eprintln!(
            "sidra reply rejected: message {in_reply_to_message_id} is not in \
             conversation {conversation_id}"
        );
        return Err(ApiError::not_found(
            "answered message not found in this conversation",
        ));
    }

    let store = get_store_by_id(pool, row.store_id).await?;
    if store.sidra_auto_reply {
        let message = insert_message(pool, conversation_id, "ai", None, reply_text).await?;
        eprintln!(
            "sidra reply accepted: auto-reply message {} in conversation {conversation_id}",
            message.id
        );
        Ok(SidraReplyResponse {
            mode: "sent".to_string(),
            message_id: Some(message.id),
            suggestion_id: None,
        })
    } else {
        let suggestion_id: Uuid = sqlx::query_scalar(
            "INSERT INTO sidra_suggestions (conversation_id, in_reply_to, body) \
             VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(conversation_id)
        .bind(in_reply_to_message_id)
        .bind(reply_text)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("store sidra suggestion", e))?;
        eprintln!(
            "sidra reply accepted: suggestion {suggestion_id} pending for \
             conversation {conversation_id}"
        );
        Ok(SidraReplyResponse {
            mode: "suggested".to_string(),
            message_id: None,
            suggestion_id: Some(suggestion_id),
        })
    }
}

// ---------------------------------------------------------------------------
// Seller SIDRA controls: suggestions queue + auto-reply mode
// ---------------------------------------------------------------------------

const SUGGESTION_SELECT: &str = "SELECT g.id, g.conversation_id, \
    g.in_reply_to AS in_reply_to_message_id, g.body, g.status, \
    g.created_at, g.decided_at, \
    COALESCE(NULLIF(u.name, ''), '') AS buyer_name, u.phone AS buyer_phone \
    FROM sidra_suggestions g \
    JOIN conversations c ON c.id = g.conversation_id \
    JOIN users u ON u.id = c.buyer_id";

#[derive(Debug, FromRow)]
struct SuggestionRow {
    id: Uuid,
    conversation_id: Uuid,
    in_reply_to_message_id: Uuid,
    body: String,
    status: String,
    created_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
    buyer_name: String,
    buyer_phone: String,
}

impl From<SuggestionRow> for SidraSuggestion {
    fn from(row: SuggestionRow) -> Self {
        let SuggestionRow {
            id,
            conversation_id,
            in_reply_to_message_id,
            body,
            status,
            created_at,
            decided_at,
            buyer_name,
            buyer_phone,
        } = row;
        let name = if buyer_name.trim().is_empty() {
            None
        } else {
            Some(buyer_name)
        };
        SidraSuggestion {
            id,
            conversation_id,
            in_reply_to_message_id,
            body,
            status,
            created_at,
            decided_at,
            buyer_display: buyer_display(&name, &buyer_phone),
        }
    }
}

/// The seller's pending SIDRA suggestions for their store,
/// oldest first (a work queue). Sent and dismissed ones leave
/// the queue; their record stays on the row.
pub async fn list_pending_suggestions(
    pool: &PgPool,
    user: &AuthUser,
) -> Result<Vec<SidraSuggestion>, ApiError> {
    let store = get_store_by_owner(pool, user.user_id).await?;
    let rows: Vec<SuggestionRow> = sqlx::query_as(&format!(
        "{SUGGESTION_SELECT} \
         WHERE c.store_id = $1 AND g.status = 'pending' \
         ORDER BY g.created_at ASC, g.id ASC"
    ))
    .bind(store.id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list sidra suggestions", e))?;
    Ok(rows.into_iter().map(SidraSuggestion::from).collect())
}

/// Load one suggestion on a conversation of the caller's own
/// store (404 otherwise — the same no-leak rule).
async fn load_suggestion_for_owner(
    pool: &PgPool,
    user: &AuthUser,
    suggestion_id: Uuid,
) -> Result<SuggestionRow, ApiError> {
    let store = get_store_by_owner(pool, user.user_id).await?;
    sqlx::query_as(&format!(
        "{SUGGESTION_SELECT} WHERE g.id = $1 AND c.store_id = $2"
    ))
    .bind(suggestion_id)
    .bind(store.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load sidra suggestion", e))?
    .ok_or_else(|| ApiError::not_found("suggestion not found"))
}

/// Reload one suggestion by id (after a decision was written).
async fn load_suggestion(pool: &PgPool, suggestion_id: Uuid) -> Result<SidraSuggestion, ApiError> {
    let row: SuggestionRow = sqlx::query_as(&format!("{SUGGESTION_SELECT} WHERE g.id = $1"))
        .bind(suggestion_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("reload sidra suggestion", e))?
        .ok_or_else(|| ApiError::not_found("suggestion not found"))?;
    Ok(row.into())
}

/// Send a pending suggestion: its body enters the conversation
/// as an `ai` message (labelled AI even though the seller
/// approved it — the words are SIDRA's), and the suggestion is
/// marked sent. A decided suggestion, or one whose conversation
/// has closed, is a 409.
pub async fn send_suggestion(
    pool: &PgPool,
    user: &AuthUser,
    suggestion_id: Uuid,
) -> Result<SidraSuggestion, ApiError> {
    let suggestion = load_suggestion_for_owner(pool, user, suggestion_id).await?;
    if suggestion.status != "pending" {
        return Err(ApiError::conflict("suggestion was already decided"));
    }
    let conversation = load_conversation_row(pool, suggestion.conversation_id).await?;
    if conversation.status != "open" {
        return Err(ApiError::conflict("conversation is closed"));
    }
    insert_message(pool, suggestion.conversation_id, "ai", None, &suggestion.body).await?;
    sqlx::query(
        "UPDATE sidra_suggestions SET status = 'sent', decided_at = now() WHERE id = $1",
    )
    .bind(suggestion_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("mark suggestion sent", e))?;
    load_suggestion(pool, suggestion_id).await
}

/// Dismiss a pending suggestion: nothing enters the
/// conversation; the row records the decision.
pub async fn dismiss_suggestion(
    pool: &PgPool,
    user: &AuthUser,
    suggestion_id: Uuid,
) -> Result<SidraSuggestion, ApiError> {
    let suggestion = load_suggestion_for_owner(pool, user, suggestion_id).await?;
    if suggestion.status != "pending" {
        return Err(ApiError::conflict("suggestion was already decided"));
    }
    sqlx::query(
        "UPDATE sidra_suggestions SET status = 'dismissed', decided_at = now() WHERE id = $1",
    )
    .bind(suggestion_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("dismiss suggestion", e))?;
    load_suggestion(pool, suggestion_id).await
}

/// Set a store's SIDRA mode: auto-reply on or off. Returns
/// the shared store view (which carries the flag). The
/// store arrives resolved by the handler — the caller's
/// own by default, or one named by `?store_id=` under
/// `manage_store_settings` (Wave 34).
pub async fn set_sidra_auto_reply(
    pool: &PgPool,
    store: &Store,
    auto_reply: bool,
) -> Result<Store, ApiError> {
    sqlx::query("UPDATE stores SET sidra_auto_reply = $2 WHERE id = $1")
        .bind(store.id)
        .bind(auto_reply)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("set sidra auto reply", e))?;
    get_store_by_id(pool, store.id).await
}

/// The seller's SIDRA status: platform bridge configuration and
/// their own store's mode, reported independently.
pub async fn sidra_status(
    state: &AppState,
    pool: &PgPool,
    user: &AuthUser,
) -> Result<SidraStatus, ApiError> {
    let store = get_store_by_owner(pool, user.user_id).await?;
    Ok(SidraStatus {
        configured: state.sidra_config.configured(),
        auto_reply: store.sidra_auto_reply,
    })
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateConversationRequest {
    pub store_id: Uuid,
    pub product_id: Option<Uuid>,
}

pub async fn create_conversation_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateConversationRequest>,
) -> Result<(StatusCode, Json<ConversationView>), ApiError> {
    let pool = state.require_pool()?;
    let (view, created) =
        find_or_create_conversation(pool, &user, req.store_id, req.product_id).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(view)))
}

pub async fn list_conversations_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<ConversationView>>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(list_conversations_for_user(pool, &user).await?))
}

pub async fn get_conversation_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ConversationThread>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(read_conversation_thread(pool, &user, id).await?))
}

#[derive(Deserialize)]
pub struct PostMessageRequest {
    pub body: String,
}

pub async fn post_message_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<PostMessageRequest>,
) -> Result<(StatusCode, Json<Message>), ApiError> {
    let pool = state.require_pool()?;
    let message = post_message(&state, pool, &user, id, &req.body).await?;
    Ok((StatusCode::CREATED, Json(message)))
}

pub async fn close_conversation_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ConversationView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(close_conversation(pool, &user, id).await?))
}

#[derive(Deserialize)]
pub struct SidraReplyRequest {
    pub conversation_id: Uuid,
    pub in_reply_to_message_id: Uuid,
    pub reply_text: String,
    pub signature: String,
}

pub async fn sidra_reply_handler(
    State(state): State<AppState>,
    Json(req): Json<SidraReplyRequest>,
) -> Result<Json<SidraReplyResponse>, ApiError> {
    let pool = state.require_pool()?;
    let response = apply_sidra_reply(
        &state,
        pool,
        req.conversation_id,
        req.in_reply_to_message_id,
        &req.reply_text,
        &req.signature,
    )
    .await?;
    Ok(Json(response))
}

pub async fn list_suggestions_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<SidraSuggestion>>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(list_pending_suggestions(pool, &user).await?))
}

pub async fn send_suggestion_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<SidraSuggestion>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(send_suggestion(pool, &user, id).await?))
}

pub async fn dismiss_suggestion_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<SidraSuggestion>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(dismiss_suggestion(pool, &user, id).await?))
}

#[derive(Deserialize)]
pub struct SetSidraAutoReplyRequest {
    pub auto_reply: bool,
}

pub async fn set_sidra_auto_reply_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<crate::access::StoreScopeQuery>,
    Json(req): Json<SetSidraAutoReplyRequest>,
) -> Result<Json<Store>, ApiError> {
    let pool = state.require_pool()?;
    // Wave 34: the toggle is store-settings work — with
    // `?store_id=` a manager may flip it for a store they
    // serve; without it, the caller's own store, as before.
    let (store, _role) = crate::access::resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        domain::STORE_ACTION_MANAGE_STORE_SETTINGS,
    )
    .await?;
    Ok(Json(
        set_sidra_auto_reply(pool, &store, req.auto_reply).await?,
    ))
}

pub async fn sidra_status_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<SidraStatus>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(sidra_status(&state, pool, &user).await?))
}
