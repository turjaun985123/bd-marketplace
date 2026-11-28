-- Wave 57: bulk import history. One row per completed
-- bulk-import run (real or dry-run) so the seller can
-- re-open the exact report the run produced — the report
-- JSON is stored verbatim (the service's BulkImportReport
-- shape), and the counts are denormalised out of it for
-- the history list. A run that failed fatally (a broken
-- header, an over-cap file) writes NO row: there is no
-- report to re-open, the request simply 400s.

CREATE TABLE bulk_imports (
    id            uuid PRIMARY KEY,
    store_id      uuid NOT NULL REFERENCES stores (id),
    actor_user_id uuid NOT NULL REFERENCES users (id),
    dry_run       boolean NOT NULL DEFAULT false,
    total_rows    integer NOT NULL,
    created_count integer NOT NULL,
    updated_count integer NOT NULL,
    error_count   integer NOT NULL,
    report        jsonb NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX bulk_imports_store_created_idx
    ON bulk_imports (store_id, created_at DESC);
