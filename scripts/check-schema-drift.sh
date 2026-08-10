#!/usr/bin/env bash
#
# Fails when the `meridian` schema contains objects no migration created.
#
# Why this exists: the legacy oracle in ../news-hub-studio shares this
# PostgreSQL server, connects as the DB role `meridian`, and runs with
# `MERIDIAN_AUTO_CREATE_SCHEMA=true`. Postgres resolves that role's default
# search_path via `"$user"` — which is the *`meridian` schema*, ours. So simply
# starting the oracle against the shared Postgres silently creates its tables,
# functions and triggers inside the Rust schema. Its search-sync triggers then
# fire on our inserts and fail, which surfaces as unexplained 500s on every
# mutating endpoint rather than as anything resembling a schema problem.
#
# This ran for real on 2026-07-30. The damage is worse than it first appears:
# beyond the trigger failures, `pg_dump` takes an ACCESS SHARE lock on *every*
# table in the schema, and the migrator role has no rights on the foreign one — so
# `make test-restore` cannot even dump, and the schema has no working backup path
# until the strays are removed. The check turns that into a one-line signal.
#
# It only ever reports. Remediation SQL is printed for a human to run, because
# dropping database objects is not something a checker should do.
#
# Usage:
#   MIGRATION_DATABASE_URL=… scripts/check-schema-drift.sh
#
# Exit 0 = schema matches the migrations. Exit 1 = drift, with the SQL to fix it.

set -euo pipefail

: "${MIGRATION_DATABASE_URL:?MIGRATION_DATABASE_URL is required}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 - <<'PYTHON'
import os, pathlib, re, subprocess, sys

url = os.environ["MIGRATION_DATABASE_URL"]

def psql(sql: str) -> list[str]:
    out = subprocess.run(
        ["psql", url, "-tAc", sql], capture_output=True, text=True, check=True
    ).stdout.strip()
    return [line for line in out.split("\n") if line]

# What the migrations declare.
expected_tables = {"_sqlx_migrations"}
for path in pathlib.Path("migrations").glob("*.sql"):
    sql = path.read_text()
    expected_tables |= set(
        re.findall(r"CREATE TABLE (?:IF NOT EXISTS )?([a-z_]+)", sql, re.I)
    )
# Migrations declare no functions or triggers today; any that appear are foreign.
expected_functions: set[str] = set()
expected_triggers: set[str] = set()

actual_tables = set(psql(
    "SELECT tablename FROM pg_tables WHERE schemaname='meridian'"
))
actual_functions = set(psql(
    "SELECT p.proname FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace "
    "WHERE n.nspname='meridian'"
))
actual_triggers = set(psql(
    "SELECT c.relname||'.'||t.tgname FROM pg_trigger t "
    "JOIN pg_class c ON c.oid=t.tgrelid "
    "JOIN pg_namespace n ON n.oid=c.relnamespace "
    "WHERE n.nspname='meridian' AND NOT t.tgisinternal"
))

stray_tables = sorted(actual_tables - expected_tables)
stray_functions = sorted(actual_functions - expected_functions)
stray_triggers = sorted(actual_triggers - expected_triggers)
missing_tables = sorted(expected_tables - actual_tables)

if missing_tables:
    print("MISSING — declared by a migration but absent from the schema:")
    for name in missing_tables:
        print(f"  - table {name}")
    print("  Run `make migrate`.\n")

if not (stray_tables or stray_functions or stray_triggers):
    if missing_tables:
        sys.exit(1)
    print(f"ok — {len(actual_tables)} tables, no foreign objects in the meridian schema.")
    sys.exit(0)

print("SCHEMA DRIFT — objects in the meridian schema that no migration creates:")
for name in stray_triggers:
    print(f"  + trigger  {name}")
for name in stray_functions:
    print(f"  + function {name}")
for name in stray_tables:
    print(f"  + table    {name}")

print()
print("The usual cause is the legacy oracle started against the shared PostgreSQL.")
print("Start it against SQLite instead — see MERIDIAN.md §9.")
print()
print("Note: while these exist, `pg_dump --schema=meridian` fails with")
print("'permission denied' (it locks every table, including the foreign one), so")
print("`make test-restore` and any schema backup are blocked, not just writes.")
print()
print("Remediation (run as the owner of these objects, then re-run this check):")
print("BEGIN;")
for name in stray_triggers:
    table, trigger = name.split(".", 1)
    print(f"  DROP TRIGGER IF EXISTS {trigger} ON meridian.{table};")
for name in stray_functions:
    print(f"  DROP FUNCTION IF EXISTS meridian.{name}() CASCADE;")
for name in stray_tables:
    print(f"  DROP TABLE IF EXISTS meridian.{name};")
print("COMMIT;")
sys.exit(1)
PYTHON
