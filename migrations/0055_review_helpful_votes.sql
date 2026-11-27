-- Wave 54 (extension): helpful votes on reviews. One row
-- is one signed-in user's "this review helped me" vote;
-- the (review, user) primary key makes a second vote the
-- same row (idempotent by construction), and un-voting
-- deletes it. A review's helpful count is always the live
-- COUNT of this table — never a stored counter that could
-- drift. Votes die with their review and with the voter.
CREATE TABLE review_helpful_votes (
    review_id uuid NOT NULL REFERENCES reviews (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (review_id, user_id)
);

CREATE INDEX review_helpful_votes_review_idx
    ON review_helpful_votes (review_id);
