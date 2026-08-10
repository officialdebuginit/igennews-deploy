#!/usr/bin/env bash
#
# Full demo seed for iGEN News — one command to make a fresh database demo-ready:
#   1. Taxonomy: 50 India Global Expo sectors + 1,348 industries (seed-sectors.sql)
#   2. Newsroom: 35-person team with complete profiles, roles, memberships, and
#      ~2 months of articles/tasks/pitches/coverage (seed-igennews.sql)
#   3. (optional) Rich article content across ALL 50 sectors via the running API
#      (seed_content_api.py) — needs the app to be up.
#
# The schema must already be migrated (migrations 0041 & 0042 add the taxonomy).
# Pass --migrate to run migrations first (needs MIGRATION_DATABASE_URL + cargo).
#
# Usage:
#   export DATABASE_DIRECT_URL=postgres://user:pass@host:5432/db   # or --db <url>
#   scripts/seed-all.sh [--migrate] [--db <url>] \
#                       [--with-content --base-url http://host:3100 \
#                        --user admin@igennews.com --pass 'DevPass123!']
#
# Idempotent: seed-sectors upserts by slug; seed-igennews re-wipes people/content
# and reseeds, PRESERVING the 50 sectors and dropping any non-IGE desk.
#
# NOTE: seed-igennews.sql is intentionally destructive to *people and editorial
# content* (TRUNCATE/DELETE). It never touches the sectors or industries.
set -euo pipefail
cd "$(dirname "$0")/.."

DB="${SEED_DATABASE_URL:-${DATABASE_DIRECT_URL:-${MIGRATION_DATABASE_URL:-}}}"
RUN_MIGRATE=0
WITH_CONTENT=0
BASE_URL="http://localhost:3100"
USER_LOGIN="admin@igennews.com"
PASS_LOGIN="DevPass123!"
PER_SECTOR=5

while [ $# -gt 0 ]; do
  case "$1" in
    --migrate)      RUN_MIGRATE=1 ;;
    --db)           DB="$2"; shift ;;
    --with-content) WITH_CONTENT=1 ;;
    --base-url)     BASE_URL="$2"; shift ;;
    --user)         USER_LOGIN="$2"; shift ;;
    --pass)         PASS_LOGIN="$2"; shift ;;
    --per-sector)   PER_SECTOR="$2"; shift ;;
    -h|--help)      sed -n '2,30p' "$0"; exit 0 ;;
    *)              echo "unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done

[ -n "$DB" ] || { echo "ERROR: no database URL. Set DATABASE_DIRECT_URL or pass --db <url>." >&2; exit 2; }
PSQL="$(command -v psql || echo /opt/homebrew/opt/libpq/bin/psql)"
[ -x "$PSQL" ] || { echo "ERROR: psql not found (tried '$PSQL')." >&2; exit 2; }

if [ "$RUN_MIGRATE" = 1 ]; then
  echo "==> [1/3] migrations"
  : "${MIGRATION_DATABASE_URL:?--migrate needs MIGRATION_DATABASE_URL}"
  cargo run -p meridian-platform --bin meridian-migrate
fi

echo "==> [2/3] taxonomy — 50 sectors + 1,348 industries"
"$PSQL" "$DB" -v ON_ERROR_STOP=1 -f scripts/seed-sectors.sql >/dev/null
echo "    ...sectors + industries seeded."

echo "==> [3/3] newsroom — team, profiles, ~2 months of content"
"$PSQL" "$DB" -v ON_ERROR_STOP=1 -f scripts/seed-igennews.sql

if [ "$WITH_CONTENT" = 1 ]; then
  echo "==> (extra) article content across all 50 sectors via the API ($BASE_URL)"
  python3 scripts/seed_content_api.py --base-url "$BASE_URL" \
    --username "$USER_LOGIN" --password "$PASS_LOGIN" \
    --only-ige --per-sector "$PER_SECTOR"
fi

echo
echo "Done. The demo is seeded."
echo "  Sign in at ${BASE_URL}/sign-in as  admin@igennews.com  /  DevPass123!"
echo "  Full account list: docs/IGENNEWS-ACCOUNTS.md"
