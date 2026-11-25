#!/bin/bash
# Fetch the HL7 FHIR JP Core package and lay it out the way sazare loads it:
#
#   <dir>/profiles/          StructureDefinition-*.json  (validation)
#   <dir>/searchparameters/  SearchParameter-*.json      (JP Core search params)
#   <dir>/.jp-core/package/  the unpacked package (examples are used by load_examples.sh)
#   <dir>/terminology/       CodeSystem-*/ValueSet-*.json  (only with --terminology)
#
# --terminology also fetches the jpfhir-terminology package (~85 MB unpacked)
# so JP-specific codes are validated: e.g. a Procedure coding in the JP
# procedure code system must be a real code of it. It adds ~60 MB of memory.
#
# sazare reads profiles/ and searchparameters/ from its working directory at
# startup, so run the server from <dir> (or mount them into the container).
#
# Usage:
#   bash examples/jp-core/setup.sh            # into the current directory
#   bash examples/jp-core/setup.sh /srv/sazare
#   JP_CORE_VERSION=1.2.0 bash examples/jp-core/setup.sh
#   bash examples/jp-core/setup.sh --terminology /srv/sazare
#
# JP Core is published by JAMI's FHIR JP implementation research group under
# CC0-1.0 (https://jpfhir.jp/fhir/core/). It is not bundled with sazare.

set -euo pipefail

TERMINOLOGY=0
if [[ "${1:-}" == "--terminology" ]]; then
  TERMINOLOGY=1
  shift
fi
DIR="${1:-.}"
VERSION="${JP_CORE_VERSION:-1.2.0}"
URL="${JP_CORE_URL:-https://jpfhir.jp/fhir/core/${VERSION}/package.tgz}"
# JP Core 1.2.0 declares jpfhir-terminology 1.4.0, which is no longer
# published; the current release resolves every value set JP Core binds.
TERM_VERSION="${JP_TERMINOLOGY_VERSION:-2.2611.0}"
TERM_URL="${JP_TERMINOLOGY_URL:-https://jpfhir.jp/fhir/core/terminology/jpfhir-terminology.r4-${TERM_VERSION}.tgz}"

mkdir -p "$DIR/profiles" "$DIR/searchparameters" "$DIR/.jp-core"
TGZ="$DIR/.jp-core/jp-core-${VERSION}.tgz"

echo "=== Downloading JP Core ${VERSION} ==="
echo "$URL"
curl -fsSL -o "$TGZ" "$URL"

rm -rf "$DIR/.jp-core/package"
tar xzf "$TGZ" -C "$DIR/.jp-core"

cp "$DIR"/.jp-core/package/StructureDefinition-*.json "$DIR/profiles/"
cp "$DIR"/.jp-core/package/SearchParameter-*.json "$DIR/searchparameters/"

if [[ "$TERMINOLOGY" == 1 ]]; then
  echo
  echo "=== Downloading jpfhir-terminology ${TERM_VERSION} ==="
  echo "$TERM_URL"
  TERM_TGZ="$DIR/.jp-core/jpfhir-terminology-${TERM_VERSION}.tgz"
  curl -fsSL -o "$TERM_TGZ" "$TERM_URL"
  rm -rf "$DIR/.jp-core/terminology" "$DIR/terminology"
  mkdir -p "$DIR/.jp-core/terminology" "$DIR/terminology"
  tar xzf "$TERM_TGZ" -C "$DIR/.jp-core/terminology"
  # JP Core's own code systems / value sets, then the terminology package's.
  cp "$DIR"/.jp-core/package/CodeSystem-*.json "$DIR"/.jp-core/package/ValueSet-*.json "$DIR/terminology/"
  cp "$DIR"/.jp-core/terminology/package/CodeSystem-*.json "$DIR"/.jp-core/terminology/package/ValueSet-*.json "$DIR/terminology/"
fi

echo
echo "profiles/:          $(ls "$DIR"/profiles/StructureDefinition-*.json | wc -l) StructureDefinitions"
echo "searchparameters/:  $(ls "$DIR"/searchparameters/SearchParameter-*.json | wc -l) SearchParameters"
if [[ "$TERMINOLOGY" == 1 ]]; then
  echo "terminology/:       $(ls "$DIR"/terminology/*.json | wc -l) CodeSystems/ValueSets"
fi
echo
echo "Next: start sazare from $DIR (it logs 'Loading N custom profiles')."
echo "Existing data is reindexed automatically on that start, so the new"
echo "search parameters cover it."
