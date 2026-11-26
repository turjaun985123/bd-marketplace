-- Wave 2: users. Phone is the primary identity (Bangladesh-first,
-- phone + OTP login). Roles: buyer (default), seller, admin.
CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE users (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    phone text UNIQUE NOT NULL,
    name text,
    role text NOT NULL DEFAULT 'buyer' CHECK (role IN ('buyer', 'seller', 'admin')),
    created_at timestamptz NOT NULL DEFAULT now()
);
