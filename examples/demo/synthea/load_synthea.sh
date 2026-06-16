#!/bin/bash
# Load a directory of Synthea-generated FHIR transaction Bundles into sazare.
#
# Synthea (https://github.com/synthetichealth/synthea, Apache-2.0) generates
# realistic synthetic patients as one transaction Bundle per patient under
# `output/fhir/`. This loads every *.json bundle in a directory and reports how
# many entries each one committed.
#
# Why a directory of bundles instead of one big file: Synthea emits per-patient
# bundles (often large, urn:uuid cross-references). sazare's transaction handler
# resolves urn:uuid references within each bundle, so loading them one at a time
# is the natural fit — and keeps the data OUT of the repo / binary.
#
# Usage:
#   bash examples/demo/synthea/load_synthea.sh /path/to/synthea/output/fhir
#
#   # Remote server with Basic auth:
#   SAZARE_URL=https://your-server SAZARE_USER=admin SAZARE_PASS=secret \
#     bash examples/demo/synthea/load_synthea.sh ./output/fhir
set -e

DIR="${1:?usage: load_synthea.sh <dir-of-synthea-bundles>}"
BASE="${SAZARE_URL:-http://localhost:8080}"

AUTH_ARGS=()
if [[ -n "$SAZARE_USER" && -n "$SAZARE_PASS" ]]; then
  AUTH_ARGS=(-u "$SAZARE_USER:$SAZARE_PASS")
fi

shopt -s nullglob
bundles=("$DIR"/*.json)
if [[ ${#bundles[@]} -eq 0 ]]; then
  echo "No *.json bundles found in $DIR" >&2
  exit 1
fi

echo "=== Loading ${#bundles[@]} Synthea bundle(s) into $BASE ==="
total_ok=0
total_entries=0
failed_files=0
for f in "${bundles[@]}"; do
  # Skip Synthea's hospital/practitioner roster bundles only if you want patients
  # only; by default we load everything so references resolve.
  resp=$(curl -s -X POST "$BASE/" "${AUTH_ARGS[@]}" \
    -H "Content-Type: application/json" --data-binary "@$f")
  read -r ok entries < <(python3 -c "
import json, sys
try:
    b = json.loads('''$resp''')
except Exception:
    print('0 0'); sys.exit()
es = b.get('entry', []) if isinstance(b, dict) else []
ok = sum(1 for e in es if str(e.get('response', {}).get('status', '')).startswith('2'))
print(ok, len(es))
" 2>/dev/null || echo "0 0")
  total_ok=$((total_ok + ok))
  total_entries=$((total_entries + entries))
  if [[ "$entries" -eq 0 || "$ok" -lt "$entries" ]]; then
    failed_files=$((failed_files + 1))
    echo "  ! $(basename "$f"): $ok/$entries committed"
  else
    echo "  ✓ $(basename "$f"): $ok/$entries"
  fi
done

echo
echo "=== Done: $total_ok/$total_entries entries committed across ${#bundles[@]} bundle(s); $failed_files file(s) with issues ==="
echo "Try:  curl -s \"$BASE/Patient?_summary=count\""
