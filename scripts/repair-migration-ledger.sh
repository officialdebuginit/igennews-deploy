#!/usr/bin/env bash
#
# Reconcile `meridian._sqlx_migrations` with a schema that is already in place.
#
# Why this exists: the ledger can end up holding fewer rows than the schema
# reflects — a restored dump, a hand-applied migration, a truncated ledger. When
# that happens `meridian-migrate` replays from the first missing version and dies
# on the first `CREATE TABLE` that already exists, so migrations are stuck.
#
# This script does NOT run any migration. For each file it:
#   1. computes the checksum sqlx would store (SHA-384 of the raw file bytes),
#   2. VERIFIES the objects that migration declares are actually present,
#   3. inserts a ledger row only for verified migrations that have none.
#
# A migration whose objects are missing is never marked applied — it is reported
# and left for `make migrate` to run properly. That is the whole safety property:
# the script can only ever record what the database already demonstrably has.
#
# Usage:
#   MIGRATION_DATABASE_URL=… scripts/repair-migration-ledger.sh [--apply]
#
# Without --apply it reports and changes nothing (the default, so a careless run
# is inert). With --apply it writes the verified rows in a single transaction.

set -euo pipefail

APPLY=0
[[ "${1:-}" == "--apply" ]] && APPLY=1

: "${MIGRATION_DATABASE_URL:?MIGRATION_DATABASE_URL is required}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 - "$APPLY" <<'PYTHON'
import hashlib, os, pathlib, re, subprocess, sys

apply_changes = sys.argv[1] == "1"
url = os.environ["MIGRATION_DATABASE_URL"]

def psql(sql: str) -> str:
    return subprocess.run(
        ["psql", url, "-tAc", sql],
        capture_output=True, text=True, check=True,
    ).stdout.strip()

tables = {t for t in psql(
    "SELECT tablename FROM pg_tables WHERE schemaname='meridian'"
).split("\n") if t}
columns = {tuple(row.split("|")) for row in psql(
    "SELECT table_name||'|'||column_name FROM information_schema.columns "
    "WHERE table_schema='meridian'"
).split("\n") if row}
indexes = {i for i in psql(
    "SELECT indexname FROM pg_indexes WHERE schemaname='meridian'"
).split("\n") if i}
recorded = {int(v) for v in psql(
    "SELECT version FROM meridian._sqlx_migrations"
).split("\n") if v}

def declared_objects(sql: str):
    """The objects a migration claims to create, as (kind, name) pairs."""
    found = []
    for name in re.findall(r"CREATE TABLE (?:IF NOT EXISTS )?([a-z_]+)", sql, re.I):
        found.append(("table", name))
    for name in re.findall(r"CREATE (?:UNIQUE )?INDEX (?:IF NOT EXISTS )?([a-z_]+)", sql, re.I):
        found.append(("index", name))
    for table, column in re.findall(
        r"ALTER TABLE ([a-z_.]+)\s+ADD COLUMN (?:IF NOT EXISTS )?([a-z_]+)", sql, re.I
    ):
        found.append(("column", f"{table.split('.')[-1]}.{column}"))
    return found

def present(kind: str, name: str) -> bool:
    if kind == "table":
        return name in tables
    if kind == "index":
        return name in indexes
    table, column = name.split(".")
    return (table, column) in columns

pending, blocked = [], []
for path in sorted(pathlib.Path("migrations").glob("*.sql")):
    version = int(path.name.split("_", 1)[0])
    description = path.name.split("_", 1)[1].removesuffix(".sql").replace("_", " ")
    raw = path.read_bytes()
    checksum = hashlib.sha384(raw).hexdigest()

    if version in recorded:
        print(f"  ok      {path.name:34s} already in the ledger")
        continue

    missing = [f"{k} {n}" for k, n in declared_objects(raw.decode()) if not present(k, n)]
    if missing:
        blocked.append((path.name, missing))
        print(f"  RUN ME  {path.name:34s} objects missing: {', '.join(missing)}")
    else:
        pending.append((version, description, checksum))
        print(f"  verify  {path.name:34s} all objects present -> will record")

print()
if blocked:
    print(f"{len(blocked)} migration(s) are genuinely unapplied and are NOT being recorded.")
    print("Run `make migrate` after this repair so they apply for real.")
    print()

if not pending:
    print("Nothing to record; the ledger already matches the schema.")
    sys.exit(0)

if not apply_changes:
    print(f"{len(pending)} row(s) would be inserted. Re-run with --apply to write them.")
    sys.exit(0)

values = ",".join(
    f"({v},'{d}',true,decode('{c}','hex'),0)" for v, d, c in pending
)
psql(
    "BEGIN; INSERT INTO meridian._sqlx_migrations "
    "(version, description, success, checksum, execution_time) VALUES "
    f"{values} ON CONFLICT (version) DO NOTHING; COMMIT;"
)
print(f"Recorded {len(pending)} migration(s).")
PYTHON
