#!/bin/sh
# Postgres first-init hook (runs ONCE, when the data volume is empty).
#
# The bare postgres image only creates the POSTGRES_USER superuser. This app
# connects as the least-privilege roles meridian_app / meridian_migrator /
# meridian_worker / meridian_readonly against the `meridian` schema, so those
# roles, the schema, its grants, and the pg_stat_statements extension must be
# provisioned before anything connects.
#
# This wraps the repository's own reviewed SQL (deploy/postgres/roles.sql and
# deploy/postgres/required-extensions.sql), which are mounted read-only at
# /provision inside the container. roles.sql requires the five *_password psql
# vars; they are supplied here from the MERIDIAN_*_PASSWORD environment.
#
# Runs as POSTGRES_USER (a superuser) over the local socket — CREATE ROLE and
# CREATE EXTENSION both need superuser.
set -eu

: "${POSTGRES_USER:?POSTGRES_USER is required}"
: "${POSTGRES_DB:?POSTGRES_DB is required}"
: "${MERIDIAN_OWNER_PASSWORD:?MERIDIAN_OWNER_PASSWORD is required}"
: "${MERIDIAN_MIGRATOR_PASSWORD:?MERIDIAN_MIGRATOR_PASSWORD is required}"
: "${MERIDIAN_APP_PASSWORD:?MERIDIAN_APP_PASSWORD is required}"
: "${MERIDIAN_WORKER_PASSWORD:?MERIDIAN_WORKER_PASSWORD is required}"
: "${MERIDIAN_READONLY_PASSWORD:?MERIDIAN_READONLY_PASSWORD is required}"

echo "provision: creating roles, schema, grants (deploy/postgres/roles.sql)"
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
  -v owner_password="$MERIDIAN_OWNER_PASSWORD" \
  -v migrator_password="$MERIDIAN_MIGRATOR_PASSWORD" \
  -v app_password="$MERIDIAN_APP_PASSWORD" \
  -v worker_password="$MERIDIAN_WORKER_PASSWORD" \
  -v readonly_password="$MERIDIAN_READONLY_PASSWORD" \
  -f /provision/roles.sql

echo "provision: enabling extensions (deploy/postgres/required-extensions.sql)"
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
  -f /provision/required-extensions.sql

echo "provision: done"
