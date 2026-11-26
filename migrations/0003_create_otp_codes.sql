-- Wave 2: OTP codes for phone login. Only a SHA-256 HASH of the code is
-- stored, never the plaintext code. At most one active row per phone:
-- requesting a new OTP deletes the previous rows for that phone, and a
-- successful verify deletes them too. Codes expire after 5 minutes and
-- allow at most 5 verify attempts (enforced in the auth service).
CREATE TABLE otp_codes (
    phone text NOT NULL,
    code_hash text NOT NULL,
    expires_at timestamptz NOT NULL,
    attempts int NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX otp_codes_phone_idx ON otp_codes (phone);
