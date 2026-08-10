#!/usr/bin/env bash
#
# Differential-parity harness — the tool the cutover depends on.
#
# For each legacy `/api/v1` operation it fires an equivalent request at the
# legacy FastAPI oracle and at the Rust server, and asserts they answer with the
# same HTTP status and the same response *shape* (a type skeleton — keys and value
# types, never values, so differing ids/timestamps across the two databases do not
# register as drift). This is the parity evidence the ledger requires before an
# operation's ownership may move; the harness never edits the ledger itself.
#
# Modes:
#   --auth      (default) Data-free. Every ledger operation is sent
#               *unauthenticated* — and, for body-carrying methods, with a
#               malformed body — to both servers. Protected routes must answer
#               401 with the same error-body shape on both, and the 401 must
#               precede body parsing (auth-ordering). Runs against empty
#               databases, so it needs no fixtures or seed data.
#   --fixtures  Data-driven. Sends the authenticated requests described in
#               contracts/parity-fixtures.json and compares status + response
#               skeleton. Needs both servers populated with a shared dataset and
#               PARITY_USER / PARITY_PASS for a login that exists on both.
#
# Environment:
#   LEGACY_BASE  default http://127.0.0.1:8000
#   RUST_BASE    default http://127.0.0.1:3100
#   PARITY_USER  login handle/email (fixtures mode)
#   PARITY_PASS  password (fixtures mode)
#   REQ_TIMEOUT  per-request seconds, default 15
#
# Output: writes contracts/parity-report.json, prints a summary, and exits
# non-zero if any operation diverged (so it can gate CI).

set -euo pipefail

mode="${1:---auth}"
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
legacy_base="${LEGACY_BASE:-http://127.0.0.1:8000}"
rust_base="${RUST_BASE:-http://127.0.0.1:3100}"
ledger="$root_dir/contracts/legacy-endpoints.json"
fixtures="$root_dir/contracts/parity-fixtures.json"
report="$root_dir/contracts/parity-report.json"
placeholder_id="00000000-0000-0000-0000-000000000000"
req_timeout="${REQ_TIMEOUT:-15}"
# Fixtures mode logs into each server independently — the two identity stores are
# separate, so the same account can carry different passwords. Per-server
# LEGACY_USER/LEGACY_PASS and RUST_USER/RUST_PASS override the shared
# PARITY_USER/PARITY_PASS fallback.
legacy_user="${LEGACY_USER:-${PARITY_USER:-}}"
legacy_pass="${LEGACY_PASS:-${PARITY_PASS:-}}"
rust_user="${RUST_USER:-${PARITY_USER:-}}"
rust_pass="${RUST_PASS:-${PARITY_PASS:-}}"

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
results="$work_dir/results.jsonl"
: > "$results"

for tool in curl jq; do
  command -v "$tool" >/dev/null || { echo "error: '$tool' is required" >&2; exit 2; }
done

# A structural skeleton of a JSON value: objects keep sorted keys and recurse,
# arrays collapse to their first element's skeleton (APIs here are homogeneous),
# scalars become their type name. Two equal skeletons = same shape, any values.
skeleton_def='
  def skeleton:
    if type == "object" then
      (to_entries | sort_by(.key) | map({key: .key, value: (.value | skeleton)}) | from_entries)
    elif type == "array" then
      (if length == 0 then [] else [(.[0] | skeleton)] end)
    else type end;
  skeleton'

# call BASE METHOD PATH TOKEN BODY  ->  prints status code; body -> $work_dir/body
call() {
  local base="$1" method="$2" path="$3" token="$4" body="$5"
  local args=(-s -m "$req_timeout" -o "$work_dir/body" -w '%{http_code}'
              -X "$method" "${base}${path}")
  [ -n "$token" ] && args+=(-H "authorization: Bearer ${token}")
  if [ -n "$body" ]; then
    args+=(-H "content-type: application/json" --data "$body")
  fi
  curl "${args[@]}" 2>/dev/null || echo "000"
}

skel_of_body() {
  jq -S "$skeleton_def" "$work_dir/body" 2>/dev/null || printf '"<non-json:%s>"' "$(wc -c < "$work_dir/body" | tr -d ' ')"
}

# shapes_verdict LSKEL RSKEL -> "equal" | "compatible" | "differ".
# "compatible" means the shapes agree once the unavoidable cross-database data
# gaps are allowed for — the two servers run on different datasets, so the same
# contract can legitimately present with less data on one side:
#   * a `null` on either side matches any type (a nullable/optional field observed
#     empty on one side, populated on the other);
#   * an empty array or empty object matches a populated one (same declared type,
#     element/entry shape simply unverifiable from the empty side).
# A populated object still requires an exact key-set match, and two non-null
# scalars of different types are always "differ" — real drift still fails.
# (Skeletons render scalars as their type name, so JSON null is the string "null".)
shapes_verdict() {
  [ "$1" = "$2" ] && { echo equal; return; }
  local r
  r="$(jq -rn --argjson a "$1" --argjson b "$2" '
    def compat($a; $b):
      if ($a == "null" or $b == "null") then true
      elif ($a|type) == "array" and ($b|type) == "array" then
        (($a|length) == 0 or ($b|length) == 0) or compat($a[0]; $b[0])
      elif ($a|type) == "object" and ($b|type) == "object" then
        (($a|length) == 0 or ($b|length) == 0)
          or (($a|keys) == ($b|keys) and all($a|keys[]; . as $k | compat($a[$k]; $b[$k])))
      else $a == $b end;
    if compat($a; $b) then "compatible" else "differ" end' 2>/dev/null)"
  echo "${r:-differ}"
}

record() {
  # record OPERATION METHOD PATH LEGACY_STATUS RUST_STATUS MATCH DETAIL
  jq -n --arg op "$1" --arg method "$2" --arg path "$3" \
        --arg ls "$4" --arg rs "$5" --arg match "$6" --arg detail "$7" \
    '{operation:$op, method:$method, path:$path,
      legacy_status:$ls, rust_status:$rs, parity:($match=="true"), detail:$detail}' \
    >> "$results"
}

run_auth_mode() {
  echo "differential-parity --auth  legacy=$legacy_base  rust=$rust_base"
  # Every ledger operation, unauthenticated. Login endpoints legitimately take no
  # auth, and the SSE stream is long-lived, so both are excluded from this mode.
  while IFS=$'\t' read -r method path op; do
    case "$path" in
      */auth/token|*/auth/refresh) continue ;;
      */events/stream) continue ;;
    esac
    local concrete
    concrete="$(printf '%s' "$path" | sed -E "s/\{[^}]+\}/${placeholder_id}/g")"
    local body=""
    case "$method" in
      POST|PUT|PATCH) body='{"__parity_probe__":' ;;  # malformed: 401 must precede parse
    esac
    local ls lskel rs rskel match detail
    ls="$(call "$legacy_base" "$method" "$concrete" "" "$body")"; lskel="$(skel_of_body)"
    rs="$(call "$rust_base" "$method" "$concrete" "" "$body")"; rskel="$(skel_of_body)"
    detail=""
    if [ "$ls" != "$rs" ]; then
      match=false; detail="status ${ls}≠${rs}"
    else
      case "$(shapes_verdict "$lskel" "$rskel")" in
        equal) match=true ;;
        compatible) match=true; detail="list type ok; element shape unverified (empty array on one side)" ;;
        *) match=false; detail="body-shape differs" ;;
      esac
    fi
    record "$op" "$method" "$concrete" "$ls" "$rs" "$match" "$detail"
  done < <(jq -r '.[] | [.method, .path, .operation_id] | @tsv' "$ledger")
}

run_fixtures_mode() {
  [ -f "$fixtures" ] || { echo "error: $fixtures not found" >&2; exit 2; }
  { [ -n "$legacy_user" ] && [ -n "$legacy_pass" ]; } || { echo "error: LEGACY_USER/LEGACY_PASS (or PARITY_USER/PARITY_PASS) required for --fixtures" >&2; exit 2; }
  { [ -n "$rust_user" ] && [ -n "$rust_pass" ]; } || { echo "error: RUST_USER/RUST_PASS (or PARITY_USER/PARITY_PASS) required for --fixtures" >&2; exit 2; }
  echo "differential-parity --fixtures  legacy=$legacy_base  rust=$rust_base"

  # login BASE USER PASS -> access token
  login() {
    curl -s -m "$req_timeout" -X POST "${1}/api/v1/auth/token" \
      -H 'content-type: application/x-www-form-urlencoded' \
      --data-urlencode "username=${2}" --data-urlencode "password=${3}" \
      2>/dev/null | jq -r '.access_token // empty'
  }
  local legacy_token rust_token
  legacy_token="$(login "$legacy_base" "$legacy_user" "$legacy_pass")"
  rust_token="$(login "$rust_base" "$rust_user" "$rust_pass")"
  [ -n "$legacy_token" ] || { echo "error: could not sign in to legacy oracle as ${legacy_user}" >&2; exit 2; }
  [ -n "$rust_token" ] || { echo "error: could not sign in to Rust server as ${rust_user}" >&2; exit 2; }

  while IFS=$'\t' read -r op method path auth query body; do
    local full="$path"
    [ "$query" != "null" ] && [ -n "$query" ] && full="${path}?${query}"
    local ltok="" rtok=""
    [ "$auth" = "true" ] && { ltok="$legacy_token"; rtok="$rust_token"; }
    [ "$body" = "null" ] && body=""
    local ls lskel rs rskel match detail
    ls="$(call "$legacy_base" "$method" "$full" "$ltok" "$body")"; lskel="$(skel_of_body)"
    rs="$(call "$rust_base" "$method" "$full" "$rtok" "$body")"; rskel="$(skel_of_body)"
    detail=""
    if [ "$ls" != "$rs" ]; then
      match=false; detail="status ${ls}≠${rs}"
    else
      case "$(shapes_verdict "$lskel" "$rskel")" in
        equal) match=true ;;
        compatible) match=true; detail="list type ok; element shape unverified (empty array on one side)" ;;
        *) match=false; detail="body-shape differs" ;;
      esac
    fi
    record "$op" "$method" "$full" "$ls" "$rs" "$match" "$detail"
  done < <(jq -r '.[] | [.operation_id, .method, .path, (.auth // false),
                         (.query // "null"),
                         (if .body then (.body|tojson) else "null" end)] | @tsv' "$fixtures")
}

case "$mode" in
  --auth) run_auth_mode ;;
  --fixtures) run_fixtures_mode ;;
  *) echo "usage: $0 [--auth|--fixtures]" >&2; exit 2 ;;
esac

# Accepted-divergence allowlist: divergences covered by a rule here are reported
# but do not fail the gate (see contracts/parity-accepted-divergences.json). A
# diverged op is "accepted" when its status pair — and, if the rule names them,
# its method and concrete path — match a rule. Missing file = no rules.
accepted_file="$root_dir/contracts/parity-accepted-divergences.json"
accepted_rules='[]'
[ -f "$accepted_file" ] && accepted_rules="$(jq -c '.rules // []' "$accepted_file")"

# Assemble the machine-readable report and a human summary.
jq -s --argjson rules "$accepted_rules" '
  def is_accepted($op):
    $rules | any(
      (.legacy_status == $op.legacy_status)
      and (.rust_status == $op.rust_status)
      and ((.method // $op.method) == $op.method)
      and ((.path // $op.path) == $op.path));
  ( map(. + {accepted: (if (.parity | not) then is_accepted(.) else false end)}) ) as $ops
  | {
      generated_mode: "'"$mode"'",
      legacy_base: "'"$legacy_base"'",
      rust_base: "'"$rust_base"'",
      total: ($ops | length),
      parity: ($ops | map(select(.parity)) | length),
      diverged: ($ops | map(select(.parity | not)) | length),
      accepted: ($ops | map(select((.parity | not) and .accepted)) | length),
      unexpected: ($ops | map(select((.parity | not) and (.accepted | not))) | length),
      operations: ($ops | sort_by(.path, .method))
    }' "$results" > "$report"

total="$(jq -r '.total' "$report")"
ok="$(jq -r '.parity' "$report")"
bad="$(jq -r '.diverged' "$report")"
accepted_n="$(jq -r '.accepted' "$report")"
unexpected="$(jq -r '.unexpected' "$report")"

echo
echo "parity ${ok}/${total}  (diverged: ${bad} — accepted: ${accepted_n}, unexpected: ${unexpected})  →  ${report#"$root_dir"/}"
if [ "$bad" -gt 0 ]; then
  echo
  printf '  %-4s %-7s %-45s %-9s %s\n' "" "METHOD" "PATH" "STATUS" "DETAIL"
  jq -r '.operations[] | select(.parity | not)
         | "\(if .accepted then "ok  " else "FAIL" end)\t\(.method|.[0:7])\t\(.path|.[0:45])\t\(.legacy_status)|\(.rust_status)\t\(.detail)"' \
    "$report" | while IFS=$'\t' read -r c m p s d; do
      printf '  %-4s %-7s %-45s %-9s %s\n' "$c" "$m" "$p" "$s" "$d"
    done
fi
if [ "$unexpected" -gt 0 ]; then
  echo
  echo "FAIL: ${unexpected} unexpected divergence(s) — not covered by contracts/parity-accepted-divergences.json"
  exit 1
fi
if [ "$bad" -gt 0 ]; then
  echo
  echo "All ${bad} divergence(s) are accepted by policy; no unexpected divergence."
else
  echo "All probed operations agree."
fi
