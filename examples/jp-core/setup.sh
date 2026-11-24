#!/bin/bash
# Fetch the HL7 FHIR JP Core package and lay it out the way sazare loads it:
#
#   <dir>/profiles/          StructureDefinition-*.json  (validation)
#   <dir>/searchparameters/  SearchParameter-*.json      (JP Core search params)
#   <dir>/.jp-core/package/  the unpacked package (examples are used by load_examples.sh)
#
# sazare reads profiles/ and searchparameters/ from its working directory at
# startup, so run the server from <dir> (or mount them into the container).
#
# Usage:
#   bash examples/jp-core/setup.sh            # into the current directory
#   bash examples/jp-core/setup.sh /srv/sazare
#   JP_CORE_VERSION=1.2.0 bash examples/jp-core/setup.sh
#
# JP Core is published by JAMI's FHIR JP implementation research group under
# CC0-1.0 (https://jpfhir.jp/fhir/core/). It is not bundled with sazare.

set -euo pipefail

DIR="${1:-.}"
VERSION="${JP_CORE_VERSION:-1.2.0}"
URL="${JP_CORE_URL:-https://jpfhir.jp/fhir/core/${VERSION}/package.tgz}"

mkdir -p "$DIR/profiles" "$DIR/searchparameters" "$DIR/.jp-core"
TGZ="$DIR/.jp-core/jp-core-${VERSION}.tgz"

echo "=== Downloading JP Core ${VERSION} ==="
echo "$URL"
curl -fsSL -o "$TGZ" "$URL"

rm -rf "$DIR/.jp-core/package"
tar xzf "$TGZ" -C "$DIR/.jp-core"

cp "$DIR"/.jp-core/package/StructureDefinition-*.json "$DIR/profiles/"
cp "$DIR"/.jp-core/package/SearchParameter-*.json "$DIR/searchparameters/"

echo
echo "profiles/:          $(ls "$DIR"/profiles/StructureDefinition-*.json | wc -l) StructureDefinitions"
echo "searchparameters/:  $(ls "$DIR"/searchparameters/SearchParameter-*.json | wc -l) SearchParameters"
echo
echo "Next: start sazare from $DIR (it logs 'Loading N custom profiles')."
echo "If the server already holds data, rebuild the index once so the new"
echo "search parameters cover it:  curl -X POST http://localhost:8080/\$reindex"
