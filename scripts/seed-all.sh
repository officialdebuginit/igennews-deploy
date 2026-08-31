#!/usr/bin/env bash
# Seed a database from empty to a complete working newsroom.
#
#   export SEED_DATABASE_URL="postgres://USER:PASS@HOST:5432/DB"   # direct, not pooled
#   scripts/seed-all.sh
#
# Order matters and is not interchangeable:
#   1. seed-sectors.sql   50 sectors + 1,348 industries, and the desks built from them.
#   2. seed-roles.sql     role_definitions, which role_assignments has a foreign key to.
#   3. seed-igennews.sql  people, content, and the full editorial trail.
#
# Every step is idempotent: re-running this script leaves identical data. The
# content seed is destructive to *people and editorial content* (it clears every
# table it writes before writing) but never touches sectors or industries.
#
# PRIVILEGES: the content seed begins with TRUNCATE, which requires table
# ownership. In a deployment that separates roles, the application role
# (DATABASE_DIRECT_URL) typically CANNOT truncate and seeding with it fails on
# the first statement. Point SEED_DATABASE_URL at the owner/migrator role.
set -euo pipefail

DB="${SEED_DATABASE_URL:-${MIGRATION_DATABASE_URL:-${DATABASE_DIRECT_URL:-}}}"
[ -n "$DB" ] || {
  echo "set SEED_DATABASE_URL (or MIGRATION_DATABASE_URL) to a direct postgres URL" >&2
  exit 1
}

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
redacted() { echo "$1" | sed 's#://[^@]*@#://***@#'; }

# Prefer a psql that is actually present; Homebrew keeps libpq off the default PATH.
PSQL="${PSQL:-}"
if [ -z "$PSQL" ]; then
  for c in psql /opt/homebrew/opt/libpq/bin/psql /usr/local/opt/libpq/bin/psql; do
    if command -v "$c" >/dev/null 2>&1; then PSQL="$c"; break; fi
  done
fi
[ -n "$PSQL" ] || { echo "psql not found; set PSQL=/path/to/psql" >&2; exit 1; }

echo "Seeding $(redacted "$DB")"

# Preflight. Both of these fail deep inside a 1.1 MB file otherwise, with an
# error that does not say which role to use instead.
if ! "$PSQL" "$DB" -At -c 'SELECT 1' >/dev/null 2>&1; then
  echo "  cannot connect to $(redacted "$DB")" >&2
  exit 1
fi
if ! "$PSQL" "$DB" -At -c "SELECT to_regclass('meridian.stories') IS NOT NULL" 2>/dev/null | grep -qx t; then
  echo "  schema not present: run the application's migrations first" >&2
  exit 1
fi
if ! "$PSQL" "$DB" -At -c \
     "SELECT has_table_privilege(current_user,'meridian.stories','TRUNCATE')" 2>/dev/null | grep -qx t; then
  who=$("$PSQL" "$DB" -At -c 'SELECT current_user' 2>/dev/null || echo '?')
  cat >&2 <<MSG
  role '$who' cannot TRUNCATE meridian.stories, and the content seed starts with
  TRUNCATE. Seed with the owner/migrator role instead:
      export SEED_DATABASE_URL="\$MIGRATION_DATABASE_URL"
MSG
  exit 1
fi

run() {
  printf '  %-22s ' "$(basename "$1")"
  "$PSQL" "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >/dev/null
  printf 'ok\n'
}

run "$HERE/seed-sectors.sql"
run "$HERE/seed-roles.sql"
run "$HERE/seed-igennews.sql"

"$PSQL" "$DB" -At -q <<'SQL'
SET search_path = meridian, public;
SELECT '  ' || count(*) || ' stories across ' || count(DISTINCT desk_id) || ' sectors' FROM stories;
SELECT '  ' || count(*) || ' people, ' || (SELECT count(*) FROM role_definitions) || ' role definitions' FROM users;
SELECT '  ' || count(*) || ' releases, ' || (SELECT count(*) FROM reviews) || ' reviews, '
              || (SELECT count(*) FROM corrections) || ' corrections' FROM releases;
SQL

echo
echo "Sign in as admin@igennews.com / DevPass123!"
