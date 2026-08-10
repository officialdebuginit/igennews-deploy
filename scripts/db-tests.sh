#!/usr/bin/env bash
# Runs the live-database integration suites — the `#[ignore]`'d tests that need
# the shared PostgreSQL stack (publishing pipeline, sector authz). They are kept
# out of the offline CI `gates` job on purpose; this is how you run them locally.
#
# Uses the DIRECT (non-PgBouncer) connection because these tests prepare
# statements, which the transaction-pooled DATABASE_URL does not support.
#
# Usage: scripts/db-tests.sh [extra `cargo test` args]
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -f .env.local ]; then
    set -a
    # shellcheck disable=SC1091
    . ./.env.local
    set +a
fi

export DATABASE_DIRECT_URL="${DATABASE_DIRECT_URL:-${MIGRATION_DATABASE_URL:-}}"
if [ -z "${DATABASE_DIRECT_URL:-}" ]; then
    echo "error: set DATABASE_DIRECT_URL (or MIGRATION_DATABASE_URL) — run scripts/provision-local-database.sh first." >&2
    exit 1
fi

# --test-threads=1: the suites share the one live database and tear down by scope.
exec cargo test --workspace -- --ignored --test-threads=1 "$@"
