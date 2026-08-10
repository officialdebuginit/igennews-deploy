#!/usr/bin/env bash
#
# Fails when the /api/v1 surface this service serves beyond the legacy oracle
# stops matching contracts/beyond-legacy-endpoints.json.
#
# Why this exists: the differential-parity harness iterates the *legacy* ledger
# (scripts/differential-parity.sh, the `while read` over contracts/legacy-endpoints.json).
# An operation the oracle does not have is therefore never probed — it cannot show
# up as a divergence, and an accepted-divergence rule for it would match nothing.
# So the beyond-legacy surface is invisible to every parity gate by construction.
# This check is what makes it visible: extending past the oracle stays possible,
# but stops being silent.
#
# Route registrations are read straight from the source rather than from a running
# server, so this needs no database and no ports — it can run in CI next to clippy.

set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ledger="$root_dir/contracts/legacy-endpoints.json"
declared="$root_dir/contracts/beyond-legacy-endpoints.json"

for f in "$ledger" "$declared"; do
  [ -f "$f" ] || { echo "missing $f" >&2; exit 2; }
done
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 2; }

python3 - "$root_dir" "$ledger" "$declared" <<'PY'
import json, re, sys, glob, os

root, ledger_path, declared_path = sys.argv[1], sys.argv[2], sys.argv[3]

legacy = {(e["method"].upper(), e["path"])
          for e in json.load(open(ledger_path))}
declared = {(o["method"].upper(), o["path"])
            for o in json.load(open(declared_path))["operations"]}

# Read `.route("<path>", <methods>)` with paren matching, so a multi-line
# registration is read as one call rather than truncated at the first newline.
served = set()
for path in sorted(glob.glob(os.path.join(root, "crates/web/src/*.rs"))):
    src = open(path).read()
    for m in re.finditer(r"\.route\(", src):
        i, depth = m.end(), 1
        while i < len(src) and depth:
            if src[i] == "(":
                depth += 1
            elif src[i] == ")":
                depth -= 1
            i += 1
        call = src[m.end():i - 1]
        pm = re.match(r'\s*"([^"]+)"', call)
        if not pm:
            continue
        route = pm.group(1)
        if not route.startswith("/api/v1"):
            continue
        for verb in re.findall(
            r"(?:axum::routing::)?\b(get|post|put|patch|delete)\s*\(", call[pm.end():]
        ):
            served.add((verb.upper(), route))

beyond = served - legacy
undeclared = sorted(beyond - declared)
stale = sorted(declared - beyond)

if not undeclared and not stale:
    print(f"beyond-legacy surface: {len(beyond)} operations, all declared")
    sys.exit(0)

if undeclared:
    print("UNDECLARED — served beyond the legacy oracle, absent from the inventory:")
    for verb, route in undeclared:
        print(f"  + {verb} {route}")
if stale:
    print("STALE — declared in the inventory, no longer served:")
    for verb, route in stale:
        print(f"  - {verb} {route}")
print()
print(f"Reconcile {os.path.relpath(declared_path, root)}: add an entry with a reason,")
print("or remove the route. Extending past the oracle is allowed; doing it silently is not.")
sys.exit(1)
PY
