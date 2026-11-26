#!/usr/bin/env bash
# BD Marketplace — database backup (Wave 31).
#
# Produces a gzipped pg_dump of the marketplace database plus a
# SHA-256 sidecar, and prunes old backups (the newest 14 are
# kept). This is ops tooling: the app never runs it — the
# operator (or cron) does.
#
# Usage:
#
#   DATABASE_URL=postgres://user:pass@host:5432/dbname \
#     ./scripts/backup.sh [outdir]
#
#   ./scripts/backup.sh --compose [outdir]
#
# Direct mode dumps through `pg_dump` on this machine and needs
# DATABASE_URL (the same value the API uses). Compose mode runs
# pg_dump INSIDE the compose stack's postgres container (service
# `postgres` in the root docker-compose.yml), using the
# POSTGRES_USER / POSTGRES_DB values the container itself was
# started with — no local client tools or credentials needed.
#
# Default outdir is ./backups (created when missing; gitignored).
#
# Scheduling (cron example — nightly at 03:17, log appended):
#
#   17 3 * * * cd /srv/bd-marketplace && DATABASE_URL=postgres://... \
#     ./scripts/backup.sh >> /var/log/bd-marketplace-backup.log 2>&1
#
# Restore summary (see scripts/restore.sh for the full flow):
#
#   DATABASE_URL=postgres://user:pass@host:5432/dbname \
#     ./scripts/restore.sh backups/backup-<timestamp>.sql.gz
#
# A backup file is only useful once it leaves this host: copy
# the .sql.gz (and its .sha256) somewhere off-site. This script
# deliberately does NOT upload anywhere — where backups live is
# the operator's call.
set -euo pipefail

KEEP=14

mode="direct"
if [[ "${1:-}" == "--compose" ]]; then
    mode="compose"
    shift
fi
outdir="${1:-./backups}"
mkdir -p "$outdir"

timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
file="$outdir/backup-$timestamp.sql.gz"
# Two runs in the same second must not overwrite each other.
n=2
while [[ -e "$file" ]]; do
    file="$outdir/backup-$timestamp-$n.sql.gz"
    n=$((n + 1))
done
tmp="$file.tmp"

if [[ "$mode" == "compose" ]]; then
    if ! command -v docker >/dev/null 2>&1; then
        echo "error: --compose mode needs docker on PATH" >&2
        exit 1
    fi
    # The container's own POSTGRES_USER / POSTGRES_DB (set from
    # the compose environment) name the database to dump.
    docker compose exec -T postgres \
        sh -c 'pg_dump -U "$POSTGRES_USER" "$POSTGRES_DB"' | gzip >"$tmp"
else
    if ! command -v pg_dump >/dev/null 2>&1; then
        echo "error: pg_dump not found on PATH — install the PostgreSQL" >&2
        echo "client tools (e.g. apt-get install postgresql-client), or" >&2
        echo "use ./scripts/backup.sh --compose against the compose stack." >&2
        exit 1
    fi
    if [[ -z "${DATABASE_URL:-}" ]]; then
        echo "error: DATABASE_URL is not set — point it at the database" >&2
        echo "to back up, or use ./scripts/backup.sh --compose." >&2
        exit 1
    fi
    pg_dump "$DATABASE_URL" | gzip >"$tmp"
fi
mv "$tmp" "$file"

if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" >"$file.sha256"
else
    shasum -a 256 "$file" >"$file.sha256"
fi

size="$(du -h "$file" | cut -f1)"
echo "backup written: $file ($size)"
echo "checksum:       $file.sha256"

# Retention: keep the newest KEEP backups, delete older ones
# together with their checksum sidecars.
shopt -s nullglob
backups=("$outdir"/backup-*.sql.gz)
if ((${#backups[@]} > KEEP)); then
    # Oldest first (by modification time), drop all but KEEP.
    mapfile -t by_age < <(ls -1tr "${backups[@]}")
    excess=$((${#by_age[@]} - KEEP))
    for ((i = 0; i < excess; i++)); do
        echo "pruning old backup: ${by_age[i]}"
        rm -f "${by_age[i]}" "${by_age[i]}.sha256"
    done
fi
