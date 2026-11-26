#!/usr/bin/env bash
# BD Marketplace — database restore (Wave 31).
#
# Restores a backup made by scripts/backup.sh into the database
# named by DATABASE_URL. This is ops tooling: the app never runs
# it — the operator does, usually into a fresh database (a
# restore drill, or a real recovery).
#
# Usage:
#
#   DATABASE_URL=postgres://user:pass@host:5432/dbname \
#     ./scripts/restore.sh [--yes] backups/backup-<timestamp>.sql.gz
#
# What happens:
#
#   1. Unless --yes is given, the script asks for confirmation.
#      A restore is destructive: the target database's `public`
#      schema is DROPPED and recreated empty before the dump is
#      loaded, so the restored database contains exactly what
#      the backup held — nothing that was there before survives.
#   2. The dump is loaded with psql ON_ERROR_STOP: the first
#      error aborts the restore loudly instead of leaving a
#      half-loaded database that looks fine.
#   3. A smoke check prints the restored public-table count.
#
# Before restoring over anything that matters, take a backup of
# the CURRENT database first (scripts/backup.sh) — a restore
# cannot be undone.
set -euo pipefail

assume_yes=0
if [[ "${1:-}" == "--yes" ]]; then
    assume_yes=1
    shift
fi

file="${1:-}"
if [[ -z "$file" ]]; then
    echo "usage: DATABASE_URL=... ./scripts/restore.sh [--yes] <backup.sql.gz>" >&2
    exit 1
fi
if [[ ! -f "$file" ]]; then
    echo "error: backup file not found: $file" >&2
    exit 1
fi
if ! command -v psql >/dev/null 2>&1; then
    echo "error: psql not found on PATH — install the PostgreSQL" >&2
    echo "client tools (e.g. apt-get install postgresql-client)." >&2
    exit 1
fi
if [[ -z "${DATABASE_URL:-}" ]]; then
    echo "error: DATABASE_URL is not set — point it at the database" >&2
    echo "to restore INTO. Its public schema will be replaced." >&2
    exit 1
fi

# Optional integrity check when the sidecar from backup.sh is
# present: a corrupt download must fail here, not halfway
# through the load.
if [[ -f "$file.sha256" ]]; then
    echo "verifying checksum: $file.sha256"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum --check "$file.sha256"
    else
        shasum -a 256 --check "$file.sha256"
    fi
fi

if [[ "$assume_yes" -ne 1 ]]; then
    echo "About to restore: $file"
    echo "Into the database named by DATABASE_URL."
    echo "Its public schema will be DROPPED and replaced by the backup."
    read -r -p "Type 'restore' to continue: " answer
    if [[ "$answer" != "restore" ]]; then
        echo "aborted — nothing was changed."
        exit 1
    fi
fi

echo "resetting the public schema..."
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -q \
    -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public;"

echo "loading $file ..."
gzip -dc "$file" | psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -q

tables="$(psql "$DATABASE_URL" -tAc \
    "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'")"
echo "restore complete: $tables public tables in the restored database."
