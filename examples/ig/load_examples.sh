#!/usr/bin/env bash
# Load the example resources of the packages fetched by setup.sh into a running
# sazare (PUT to Type/id, so ids and the references between them are kept).
# Exits non-zero if any example is refused.
#
# usage:
#   bash examples/ig/load_examples.sh [dir]        # dir: where setup.sh laid it out (default .)
#   SAZARE_URL=http://localhost:8080 SAZARE_USER=admin SAZARE_PASS=... bash examples/ig/load_examples.sh
set -euo pipefail

DIR="${1:-.}"
BASE="${SAZARE_URL:-http://localhost:8080}"
AUTH_ARGS=()
if [[ -n "${SAZARE_USER:-}" ]]; then
  AUTH_ARGS=(-u "$SAZARE_USER:${SAZARE_PASS:-}")
fi

shopt -s nullglob
files=("$DIR"/.ig/*/package/example/*.json)
[[ ${#files[@]} -gt 0 ]] || { echo "No examples under $DIR/.ig — run examples/ig/setup.sh first." >&2; exit 1; }

echo "=== Loading ${#files[@]} examples into $BASE ==="
ok=0; failed=0
for f in "${files[@]}"; do
  target=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d["resourceType"]+"/"+d["id"])' "$f")
  code=$(curl -s -o /dev/null -w '%{http_code}' "${AUTH_ARGS[@]}" -X PUT \
    -H 'Content-Type: application/fhir+json' --data-binary @"$f" "$BASE/$target")
  if [[ "$code" -lt 300 ]]; then
    ok=$((ok + 1))
  else
    failed=$((failed + 1))
    echo "  $code $target"
  fi
done
echo "loaded: $ok, refused: $failed"
[[ "$failed" -eq 0 ]]
