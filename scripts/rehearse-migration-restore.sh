#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
legacy_env="$root_dir/../news-hub-studio/infra/.env"
local_env="$root_dir/.env.local"
probe_db="meridian_restore_probe_${$}_${RANDOM}"
work_dir="$(mktemp -d)"

if [[ ! "$probe_db" =~ ^meridian_restore_probe_[0-9]+_[0-9]+$ ]]; then
  echo 'refusing unsafe restore-probe database name' >&2
  exit 1
fi

cleanup() {
  if [[ -n "${POSTGRES_URL:-}" && "$probe_db" =~ ^meridian_restore_probe_[0-9]+_[0-9]+$ ]]; then
    dropdb --if-exists --force --maintenance-db="$POSTGRES_URL" "$probe_db" >/dev/null
  fi
  rm -rf "$work_dir"
}
trap cleanup EXIT

test -f "$local_env" || {
  echo 'run make provision-local first' >&2
  exit 1
}

set -a
source "$legacy_env"
source "$local_env"
set +a

pg_dump "$MIGRATION_DATABASE_URL" \
  --format=custom --schema=meridian --no-owner --no-privileges \
  --file="$work_dir/meridian.dump"

createdb --maintenance-db="$POSTGRES_URL" --template=template0 "$probe_db"
probe_admin_url="${POSTGRES_URL%/*}/$probe_db"
probe_migration_url="${MIGRATION_DATABASE_URL%/*}/$probe_db"

psql "$probe_admin_url" -X -v ON_ERROR_STOP=1 \
  -c "GRANT CREATE ON DATABASE \"$probe_db\" TO meridian_migrator" >/dev/null
pg_restore --dbname="$probe_admin_url" --role=meridian_migrator \
  --no-owner --no-privileges "$work_dir/meridian.dump"

MIGRATION_DATABASE_URL="$probe_migration_url" \
  cargo run --manifest-path "$root_dir/Cargo.toml" \
  -p meridian-platform --bin meridian-migrate >/dev/null

ledger_count="$(psql "$probe_migration_url" -X -Atc \
  'SELECT count(*) FROM meridian._sqlx_migrations WHERE success')"
table_count="$(psql "$probe_migration_url" -X -Atc \
  "SELECT count(*) FROM information_schema.tables WHERE table_schema='meridian' AND table_name='rust_platform_migrations'")"
# Every migration file must be recorded as applied on the restored copy. This was
# previously hardcoded to 1, which silently asserted a single-row ledger — so a
# ledger that had lost rows read as healthy. Comparing against the file count
# means the rehearsal fails when the restored ledger and the migration set drift.
expected_ledger="$(find "$root_dir/migrations" -name '*.sql' | wc -l | tr -d ' ')"

if [[ "$ledger_count" != "$expected_ledger" || "$table_count" != "1" ]]; then
  echo "restored migration state failed validation" >&2
  echo "  ledger rows: $ledger_count (expected $expected_ledger)" >&2
  echo "  platform table present: $table_count (expected 1)" >&2
  exit 1
fi

echo 'snapshot restore and forward-migration rehearsal passed; probe database removed'
