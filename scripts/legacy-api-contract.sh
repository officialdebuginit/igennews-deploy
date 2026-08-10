#!/usr/bin/env bash
set -euo pipefail

mode="${1:---check}"
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
openapi_url="${LEGACY_OPENAPI_URL:-http://127.0.0.1:8000/openapi.json}"
contract_dir="$root_dir/contracts"
snapshot="$contract_dir/legacy-openapi.json"
ledger="$contract_dir/legacy-endpoints.json"
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

curl --fail --silent --show-error --max-time 15 "$openapi_url" \
  | jq -S . > "$work_dir/openapi.json"

jq '
  def operation: IN("get", "post", "put", "patch", "delete");
  def risk($method; $path):
    if ($path | contains("/auth/"))
      or ($path | contains("/stories"))
      or ($path | contains("/releases"))
      or ($path | contains("/assets"))
      or ($path | contains("/events/stream")) then "high"
    elif $method == "GET" then "low"
    else "medium"
    end;
  [
    .paths | to_entries[] as $path
    | $path.value | to_entries[]
    | select(.key | operation)
    | {
        method: (.key | ascii_upcase),
        path: $path.key,
        operation_id: .value.operationId,
        domains: (.value.tags // []),
        openapi_declares_auth: ((.value.security // []) | length > 0),
        request_content_types: ((.value.requestBody.content // {}) | keys | sort),
        response_codes: (.value.responses | keys | sort),
        migration_risk: risk((.key | ascii_upcase); $path.key),
        owner: "legacy",
        migration_status: "not_started"
      }
  ] | sort_by(.path, .method)
' "$work_dir/openapi.json" > "$work_dir/endpoints.json"

case "$mode" in
  --update)
    mkdir -p "$contract_dir"
    cp "$work_dir/openapi.json" "$snapshot"
    cp "$work_dir/endpoints.json" "$ledger"
    ;;
  --check)
    cmp --silent "$work_dir/openapi.json" "$snapshot" || {
      echo "legacy OpenAPI contract drift detected; inspect it before running --update" >&2
      exit 1
    }
    cmp --silent "$work_dir/endpoints.json" "$ledger" || {
      echo "legacy endpoint ledger drift detected; inspect it before running --update" >&2
      exit 1
    }
    ;;
  *)
    echo "usage: $0 [--check|--update]" >&2
    exit 2
    ;;
esac
