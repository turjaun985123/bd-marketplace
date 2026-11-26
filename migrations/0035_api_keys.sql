-- Wave 37: third-party / mobile API keys.
--
-- A key is a long-lived bearer credential belonging to ONE
-- user; a request authenticated with it acts as that user,
-- narrowed by the key's scopes. Keys exist for external
-- apps (a future mobile app, a seller's own integration)
-- that cannot run the phone + OTP + JWT login flow.
--
-- Only the SHA-256 hash of the full key is stored
-- (`key_hash`, UNIQUE — the auth lookup is by hash, so the
-- unique index IS the lookup index). The full key is shown
-- to the owner exactly once, in the creation response; it
-- can never be read back. `key_prefix` (the first 12
-- characters, marker included) is stored in the clear so
-- key lists can tell keys apart without exposing anything
-- usable.
--
-- `scopes` is the comma-joined canonical scope set the
-- domain validator produces: 'read' or 'read,write'. The
-- write scope implies read. Scope enforcement and the
-- JWT-only route exclusions live in the API's auth layer,
-- not in this table.
--
-- Lifecycle: a key is usable while `revoked_at` IS NULL
-- and (`expires_at` IS NULL or `expires_at` is in the
-- future). Revocation is a timestamp, never a delete — the
-- row stays as the record that the key existed.
-- `last_used_at` is stamped best-effort on each successful
-- key authentication.

CREATE TABLE api_keys (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name text NOT NULL,
    key_prefix text NOT NULL,
    key_hash text NOT NULL UNIQUE,
    scopes text NOT NULL DEFAULT 'read',
    last_used_at timestamptz,
    expires_at timestamptz,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Serves the owner's key list (newest first per user).
CREATE INDEX api_keys_user_created_idx
    ON api_keys (user_id, created_at DESC);
