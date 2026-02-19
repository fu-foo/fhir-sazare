#!/bin/bash
# _summary and _elements filtering
# Usage: bash examples/06_summary_elements.sh
#
# Prerequisites: Server running at http://localhost:8080 with some Patient data
#   (run 02_search.sh first to create test data)

BASE=http://localhost:8080

echo "=== 1. Normal read (full resource) ==="
# Get first Patient ID
PID=$(curl -s "$BASE/Patient?_count=1" | python3 -c "
import sys,json
b=json.load(sys.stdin)
entries=b.get('entry',[])
if entries: print(entries[0]['resource']['id'])
else: print('none')
" 2>/dev/null)

if [ "$PID" = "none" ]; then
  echo "No patients found. Run 02_search.sh first."
  exit 1
fi

curl -s "$BASE/Patient/$PID" | python3 -m json.tool
echo

echo "=== 2. _summary=true (summary fields only) ==="
curl -s "$BASE/Patient/$PID?_summary=true" | python3 -m json.tool
echo

echo "=== 3. _summary=text (text + id + meta only) ==="
curl -s "$BASE/Patient/$PID?_summary=text" | python3 -m json.tool
echo

echo "=== 4. _summary=count (search: total count only, no entries) ==="
curl -s "$BASE/Patient?_summary=count" | python3 -m json.tool
echo

echo "=== 5. _elements=name,gender (specific fields only) ==="
curl -s "$BASE/Patient/$PID?_elements=name,gender" | python3 -m json.tool
echo

echo "=== 6. Search with _elements ==="
curl -s "$BASE/Patient?_elements=name,birthDate" | python3 -m json.tool
echo
