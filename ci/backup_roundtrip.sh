#!/usr/bin/env bash
# Backup → restore round trip, as the README describes it:
#   1. start a server and load data
#   2. take a backup with `--backup` while the server is running
#   3. restore into a fresh data_dir: resources.sqlite + audit.sqlite only
#      (the search index and full text are left out — they are rebuilt)
#   4. start on the restored data and compare counts and searches
#
# usage: ci/backup_roundtrip.sh   (env: SAZARE_BIN, PORT)
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
BIN=${SAZARE_BIN:-$HERE/../target/release/sazare-server}
PORT=${PORT:-18190}
BASE="http://127.0.0.1:$PORT"
WORK=$(mktemp -d)
trap 'kill "${PID:-}" 2>/dev/null || true; rm -rf "$WORK"' EXIT

config() {
  cat > "$WORK/$1.yaml" <<YAML
server:
  host: "127.0.0.1"
  port: $PORT
storage:
  data_dir: "$WORK/$1"
log:
  level: "warn"
YAML
}

start() {
  "$BIN" run --config "$WORK/$1.yaml" > "$WORK/$1.log" 2>&1 &
  PID=$!
  # Up, and the search index ready: a restored data dir has no index, and the
  # server answers while it rebuilds one in the background.
  for _ in $(seq 600); do
    curl -sf "$BASE/health" 2>/dev/null | grep -q '"state":"ready"' && return
    sleep 0.2
  done
  cat "$WORK/$1.log"; exit 1
}

stop() { kill "$PID"; wait "$PID" 2>/dev/null || true; PID=; }

# What we compare before and after: counts and a few searches.
snapshot() {
  for q in "Patient?_summary=count" "Observation?_summary=count" \
           "Patient?family=Fam1" "Observation?code=http://loinc.org|8867-4" \
           "Patient?_has:Observation:subject:code=8867-4" \
           "Observation?_content=8867-4" "Observation?date=ge2022-01-01&_summary=count"; do
    printf '%s %s\n' "$q" "$(curl -s "$BASE/$q" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("total", d.get("resourceType")))')"
  done
}

config original
start original
python3 "$HERE/../bench/gen_data.py" 300 3 > "$WORK/data.ndjson"
curl -sf -X POST -H 'Content-Type: application/fhir+ndjson' --data-binary @"$WORK/data.ndjson" "$BASE/\$import" > /dev/null
# A second version of one resource, so history is part of the comparison.
pid=$(curl -s "$BASE/Patient?_count=1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["entry"][0]["resource"]["id"])')
curl -s "$BASE/Patient/$pid" | python3 -c 'import json,sys; r=json.load(sys.stdin); r["active"]=True; print(json.dumps(r))' \
  | curl -sf -X PUT -H 'Content-Type: application/fhir+json' --data-binary @- "$BASE/Patient/$pid" > /dev/null
snapshot > "$WORK/before.txt"
curl -s "$BASE/Patient/$pid/_history" | python3 -c 'import json,sys; print("history", json.load(sys.stdin)["total"])' >> "$WORK/before.txt"

# Online backup while the server is serving.
"$BIN" backup "$WORK/backup" --config "$WORK/original.yaml"
stop

mkdir -p "$WORK/restored"
cp "$WORK/backup/resources.sqlite" "$WORK/backup/audit.sqlite" "$WORK/restored/"
config restored
start restored
snapshot > "$WORK/after.txt"
curl -s "$BASE/Patient/$pid/_history" | python3 -c 'import json,sys; print("history", json.load(sys.stdin)["total"])' >> "$WORK/after.txt"
stop

echo "--- before / after"
paste -d'|' "$WORK/before.txt" "$WORK/after.txt"
if ! diff -u "$WORK/before.txt" "$WORK/after.txt"; then
  echo "::error::restored server differs from the original"
  exit 1
fi
if grep -q ' 0$' "$WORK/before.txt"; then
  echo "::error::a comparison query matched nothing — the check would prove nothing"
  exit 1
fi
echo "backup round trip OK"
