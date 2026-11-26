-- Wave 28: security hardening — the OTP request log behind the
-- per-phone request throttle, and the admin audit log.
--
-- otp_codes cannot feed the throttle: it holds at most ONE row
-- per phone (a new request deletes the previous rows, a
-- successful verify deletes them too), so "requests in the last
-- hour" is unanswerable from it. otp_request_log is append-only:
-- the request-otp service inserts one row per issued request and
-- counts the phone's rows inside the rolling hour. Rows are
-- never deleted by the OTP flow; an operator may prune old rows
-- freely (anything older than an hour no longer counts).
CREATE TABLE otp_request_log (
    phone text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX otp_request_log_phone_created_idx
    ON otp_request_log (phone, created_at DESC);

-- One row per consequential admin action, written by the ONE
-- writer (api::audit::record_admin_action) inside the SAME
-- transaction as the action itself, so the log can never claim
-- an action that rolled back — or miss one that committed.
-- `detail` carries non-secret context only (changed setting
-- keys, the granted tier, transition endpoints): passwords,
-- tokens, OTP codes and full NID numbers are never written here.
CREATE TABLE admin_audit_log (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    admin_user_id uuid NOT NULL REFERENCES users (id),
    action text NOT NULL,
    target_type text NOT NULL,
    target_id text,
    detail jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX admin_audit_log_created_idx
    ON admin_audit_log (created_at DESC);
