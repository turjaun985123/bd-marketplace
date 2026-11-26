-- Wave 13: in-platform messaging + the SIDRA bridge.
-- Conversations are buyer <-> store threads (optionally about one
-- product); messages carry a sender_type of buyer / seller / ai.
-- There is deliberately NO unique constraint on
-- (store_id, buyer_id, product_id): product_id is nullable, and
-- Postgres treats NULLs as distinct in unique indexes, so the
-- database cannot express "one OPEN conversation per triple" —
-- the messaging service enforces it with a lookup-then-insert
-- (see the api messaging module). The read markers drive the
-- per-side unread counts: a side's unread messages are the other
-- side's messages created after that side last opened the thread.
-- sidra_suggestions holds AI replies SIDRA sent for stores that
-- did NOT enable auto-reply: a seller reviews each one and either
-- sends it (it becomes an ai message) or dismisses it.
-- stores.sidra_auto_reply is the per-store switch between those
-- two modes.

CREATE TABLE conversations (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    store_id uuid NOT NULL REFERENCES stores (id),
    buyer_id uuid NOT NULL REFERENCES users (id),
    product_id uuid REFERENCES products (id),
    status text NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'closed')),
    buyer_last_read_at timestamptz,
    seller_last_read_at timestamptz,
    last_message_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX conversations_store_idx
    ON conversations (store_id, last_message_at DESC);
CREATE INDEX conversations_buyer_idx
    ON conversations (buyer_id, last_message_at DESC);

CREATE TABLE messages (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    conversation_id uuid NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    sender_type text NOT NULL CHECK (sender_type IN ('buyer', 'seller', 'ai')),
    sender_id uuid REFERENCES users (id),
    body text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX messages_conversation_idx
    ON messages (conversation_id, created_at);

CREATE TABLE sidra_suggestions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    conversation_id uuid NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    in_reply_to uuid NOT NULL REFERENCES messages (id),
    body text NOT NULL,
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'sent', 'dismissed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    decided_at timestamptz
);

CREATE INDEX sidra_suggestions_conversation_idx
    ON sidra_suggestions (conversation_id, status);

ALTER TABLE stores ADD COLUMN sidra_auto_reply boolean NOT NULL DEFAULT false;
