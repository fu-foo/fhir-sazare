#!/usr/bin/env bash
# Search benchmark: start a throwaway sazare server, $import a synthetic
# dataset, time a fixed set of searches, and print a Markdown table.
#
# usage: bench/run.sh [patients] [obs-per-patient]
#   env: SAZARE_BIN   server binary (default target/release/sazare-server)
#        MAX_MS       fail if any query's median exceeds this (default: no limit)
#        PORT         server port (default 18080)
set -euo pipefail

PATIENTS=${1:-10000}
OBS_PER=${2:-5}
HERE=$(cd "$(dirname "$0")" && pwd)
BIN=${SAZARE_BIN:-$HERE/../target/release/sazare-server}
PORT=${PORT:-18080}
BASE="http://127.0.0.1:$PORT"
WORK=$(mktemp -d)
trap 'kill "${SERVER_PID:-}" 2>/dev/null || true; rm -rf "$WORK"' EXIT

cat > "$WORK/config.yaml" <<YAML
server:
  host: "127.0.0.1"
  port: $PORT
auth:
  enabled: false
storage:
  data_dir: "$WORK/data"
log:
  level: "warn"
YAML
"$BIN" run --config "$WORK/config.yaml" > "$WORK/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 100); do curl -sf "$BASE/health" > /dev/null && break; sleep 0.1; done
curl -sf "$BASE/health" > /dev/null || { cat "$WORK/server.log"; exit 1; }

python3 "$HERE/gen_data.py" "$PATIENTS" "$OBS_PER" > "$WORK/data.ndjson"
TOTAL=$(wc -l < "$WORK/data.ndjson")
split -l 5000 "$WORK/data.ndjson" "$WORK/chunk_"
start=$(date +%s.%N)
for f in "$WORK"/chunk_*; do
  curl -sf -X POST "$BASE/\$import" -H 'Content-Type: application/fhir+ndjson' \
    --data-binary "@$f" > /dev/null
done
import_s=$(echo "$(date +%s.%N) - $start" | bc)

echo "### sazare search benchmark"
echo
echo "$PATIENTS Patients, $((PATIENTS * OBS_PER)) Observations."
printf 'Import: %d resources in %.1fs (%.0f/s)\n\n' "$TOTAL" "$import_s" "$(echo "$TOTAL / $import_s" | bc -l)"
printf 'Data dir: %s\n\n' "$(du -sh "$WORK/data" | cut -f1)"
echo "| median ms | total | query |"
echo "|---:|---:|---|"

fail=0
while read -r q; do
  [[ -z "$q" || "$q" == \#* ]] && continue
  times=()
  for _ in 1 2 3 4 5; do
    times+=("$(curl -s -o "$WORK/last.json" -w '%{time_total}' "$BASE/$q")")
  done
  med=$(printf '%s\n' "${times[@]}" | sort -n | sed -n 3p)
  ms=$(echo "$med * 1000" | bc)
  total=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("total", "-"))' "$WORK/last.json")
  printf '| %.1f | %s | `%s` |\n' "$ms" "$total" "${q//|/\\|}"
  if [[ -n "${MAX_MS:-}" ]] && (( $(echo "$ms > $MAX_MS" | bc) )); then
    echo "::error::$q took ${ms}ms (> ${MAX_MS}ms)" >&2
    fail=1
  fi
done < "$HERE/queries.txt"
exit $fail
