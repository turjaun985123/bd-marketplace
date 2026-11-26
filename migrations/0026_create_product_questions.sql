-- Wave 25: product Q&A. Any signed-in user may ask a question on a
-- publicly visible product (never on their own store's product);
-- the owner of the product's store answers publicly on the same
-- row — re-answering simply updates the answer (a correction, not
-- a second answer). `is_hidden` is the moderation flag: owners and
-- admins can hide a question (spam, abuse), which removes it from
-- the public list and from the dashboard's unanswered count while
-- keeping it visible in the seller inbox so it can be unhidden.
-- The asker's identity is never stored beyond the user FK; public
-- views show only their masked phone (see the API's qna module).

CREATE TABLE product_questions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    asker_user_id uuid NOT NULL REFERENCES users (id),
    body text NOT NULL,
    answer_body text,
    answered_by_user_id uuid REFERENCES users (id),
    answered_at timestamptz,
    is_hidden boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX product_questions_product_id_idx ON product_questions (product_id, created_at DESC);
CREATE INDEX product_questions_asker_id_idx ON product_questions (asker_user_id);
