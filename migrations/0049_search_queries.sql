-- Wave 53: the search-analytics log — one append-only row
-- per `GET /v1/search` call: the query text (NULL for a
-- pure filter browse), the result count the shopper got,
-- and a fingerprint of the validated filter set (the
-- domain's canonical-rendering hash).
--
-- Deliberately NO user id, session, IP or any other
-- identifier column: the operator questions this table
-- answers — "what do shoppers search for, and what do we
-- not stock?" — need counts per query text, never a
-- person's trail. Rows are only inserted (by the search
-- handler, best-effort) and aggregated (by the admin
-- analytics read).

CREATE TABLE search_queries (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    query_text text,
    result_count bigint NOT NULL,
    filters_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- The analytics read aggregates the recent window.
CREATE INDEX search_queries_created
    ON search_queries (created_at);

-- The top / zero-result reads group by text inside the
-- window.
CREATE INDEX search_queries_text_created
    ON search_queries (query_text, created_at);
