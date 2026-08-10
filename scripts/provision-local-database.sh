#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
legacy_root="$root_dir/../news-hub-studio"
legacy_env="$legacy_root/infra/.env"
local_env="$root_dir/.env.local"
compose_override="$root_dir/deploy/compose/pgbouncer-auth-query.override.yml"

set -a
source "$legacy_env"
set +a

if [[ ! -f "$local_env" ]]; then
  umask 077
  owner_password="$(openssl rand -hex 32)"
  migrator_password="$(openssl rand -hex 32)"
  app_password="$(openssl rand -hex 32)"
  worker_password="$(openssl rand -hex 32)"
  readonly_password="$(openssl rand -hex 32)"
  {
    printf 'MERIDIAN_OWNER_PASSWORD=%s\n' "$owner_password"
    printf 'MERIDIAN_MIGRATOR_PASSWORD=%s\n' "$migrator_password"
    printf 'MERIDIAN_APP_PASSWORD=%s\n' "$app_password"
    printf 'MERIDIAN_WORKER_PASSWORD=%s\n' "$worker_password"
    printf 'MERIDIAN_READONLY_PASSWORD=%s\n' "$readonly_password"
    printf 'DATABASE_URL=postgresql://meridian_app:%s@127.0.0.1:%s/%s\n' "$app_password" "$PGBOUNCER_PORT" "$POSTGRES_DB"
    printf 'DATABASE_DIRECT_URL=postgresql://meridian_app:%s@127.0.0.1:%s/%s\n' "$app_password" "$POSTGRES_PORT" "$POSTGRES_DB"
    printf 'MIGRATION_DATABASE_URL=postgresql://meridian_migrator:%s@127.0.0.1:%s/%s\n' "$migrator_password" "$POSTGRES_PORT" "$POSTGRES_DB"
    printf 'WORKER_DATABASE_URL=postgresql://meridian_worker:%s@127.0.0.1:%s/%s\n' "$worker_password" "$POSTGRES_PORT" "$POSTGRES_DB"
    printf 'READONLY_DATABASE_URL=postgresql://meridian_readonly:%s@127.0.0.1:%s/%s\n' "$readonly_password" "$POSTGRES_PORT" "$POSTGRES_DB"
    printf 'S3_ENDPOINT_URL=%s\n' "$S3_ENDPOINT_URL"
    printf 'S3_BUCKET=%s\n' "$S3_BUCKET"
    printf 'S3_ACCESS_KEY_ID=%s\n' "$S3_ACCESS_KEY_ID"
    printf 'S3_SECRET_ACCESS_KEY=%s\n' "$S3_SECRET_ACCESS_KEY"
    printf 'S3_REGION=%s\n' "$S3_REGION"
    printf 'RUST_LOG=meridian=debug,tower_http=info\n'
  } > "$local_env"
fi

if ! grep -q '^MERIDIAN_AUTH_SECRET=' "$local_env"; then
  umask 077
  printf 'MERIDIAN_AUTH_SECRET=%s\n' "$(openssl rand -hex 64)" >> "$local_env"
fi

set -a
source "$local_env"
set +a

psql "$POSTGRES_URL" -X \
  -v owner_password="$MERIDIAN_OWNER_PASSWORD" \
  -v migrator_password="$MERIDIAN_MIGRATOR_PASSWORD" \
  -v app_password="$MERIDIAN_APP_PASSWORD" \
  -v worker_password="$MERIDIAN_WORKER_PASSWORD" \
  -v readonly_password="$MERIDIAN_READONLY_PASSWORD" \
  -f "$root_dir/deploy/postgres/roles.sql"

docker compose \
  -f "$legacy_root/infra/docker-compose.yml" \
  -f "$compose_override" \
  --env-file "$legacy_env" \
  up -d --force-recreate --wait --wait-timeout 60 pgbouncer

cargo run --manifest-path "$root_dir/Cargo.toml" \
  -p meridian-platform --bin meridian-migrate

ledger_runtime_privileges="$(psql "$POSTGRES_URL" -X -Atc \
  "SELECT has_table_privilege('meridian_app', 'meridian._sqlx_migrations', 'SELECT,INSERT,UPDATE,DELETE')
       OR has_table_privilege('meridian_worker', 'meridian._sqlx_migrations', 'SELECT,INSERT,UPDATE,DELETE')
       OR has_table_privilege('meridian_readonly', 'meridian._sqlx_migrations', 'SELECT')")"
if [[ "$ledger_runtime_privileges" != "f" ]]; then
  echo 'a runtime role can access the SQLx migration ledger' >&2
  exit 1
fi

psql "$DATABASE_URL" -X -v ON_ERROR_STOP=1 -c 'SELECT 1' >/dev/null
psql "$DATABASE_DIRECT_URL" -X -v ON_ERROR_STOP=1 \
  -c "SELECT pg_notify('meridian_module_1_provisioning', 'ready')" >/dev/null
psql "$READONLY_DATABASE_URL" -X -v ON_ERROR_STOP=1 \
  -c 'SELECT count(*) FROM meridian.rust_platform_migrations' >/dev/null
psql "$MIGRATION_DATABASE_URL" -X -v ON_ERROR_STOP=1 \
  -c 'BEGIN; CREATE TABLE meridian.__migrator_ddl_probe(id integer); ROLLBACK;' >/dev/null

if psql "$DATABASE_DIRECT_URL" -X -v ON_ERROR_STOP=1 \
  -c 'CREATE TABLE meridian.__runtime_role_must_not_create(id integer)' >/dev/null 2>&1; then
  psql "$POSTGRES_URL" -X -v ON_ERROR_STOP=1 \
    -c 'DROP TABLE IF EXISTS meridian.__runtime_role_must_not_create' >/dev/null
  echo 'runtime app role unexpectedly has DDL privilege' >&2
  exit 1
fi

if psql "$READONLY_DATABASE_URL" -X -v ON_ERROR_STOP=1 \
  -c "BEGIN; INSERT INTO meridian.rust_platform_migrations(id, build_version) VALUES ('readonly-denial-probe', 'none'); ROLLBACK;" \
  >/dev/null 2>&1; then
  echo 'readonly role unexpectedly has write privilege' >&2
  exit 1
fi

echo 'local role provisioning, migration, pool authentication, and privilege checks passed'
