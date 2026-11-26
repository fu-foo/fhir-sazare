#!/usr/bin/env bash
# Fetch FHIR implementation-guide packages and lay them out the way sazare
# loads them at startup (from its working directory):
#
#   <dir>/profiles/          StructureDefinition-*.json   (profile validation)
#   <dir>/searchparameters/  SearchParameter-*.json       (the IG's search parameters)
#   <dir>/terminology/       CodeSystem-*.json, ValueSet-*.json (with --terminology)
#   <dir>/.ig/<n>/package/   each unpacked package (its examples: load_examples.sh)
#
# usage:
#   bash examples/ig/setup.sh [--dir <dir>] [--terminology] <package-url>... \
#        [--terminology-package <url>]...
#
#   --dir <dir>                 where to lay it out (default: the current directory)
#   --terminology               also load the packages' CodeSystems / ValueSets, so
#                               required bindings are validated
#   --terminology-package <url> a package to take only CodeSystems / ValueSets from
#                               (an IG's separately published terminology)
#
# example (JP Core with its terminology):
#   bash examples/ig/setup.sh --dir /srv/sazare --terminology \
#     https://jpfhir.jp/fhir/core/1.2.0/package.tgz \
#     --terminology-package https://jpfhir.jp/fhir/core/terminology/jpfhir-terminology.r4-2.2611.0.tgz
#
# Packages are downloaded from where you point; none is bundled with sazare.
set -euo pipefail

DIR=.
TERMINOLOGY=0
PACKAGES=()
TERM_PACKAGES=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dir) DIR="$2"; shift 2 ;;
    --terminology) TERMINOLOGY=1; shift ;;
    --terminology-package) TERM_PACKAGES+=("$2"); TERMINOLOGY=1; shift 2 ;;
    -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
    -*) echo "unknown option $1" >&2; exit 2 ;;
    *) PACKAGES+=("$1"); shift ;;
  esac
done
[[ ${#PACKAGES[@]} -gt 0 ]] || { echo "usage: setup.sh [--dir <dir>] [--terminology] <package-url>..." >&2; exit 2; }

mkdir -p "$DIR/profiles" "$DIR/searchparameters" "$DIR/.ig"
[[ "$TERMINOLOGY" == 1 ]] && mkdir -p "$DIR/terminology"

# fetch <url> <n> → unpacks into $DIR/.ig/<n>/package
fetch() {
  local url="$1" n="$2"
  echo "=== $url"
  rm -rf "$DIR/.ig/$n"
  mkdir -p "$DIR/.ig/$n"
  curl -fsSL -o "$DIR/.ig/$n/package.tgz" "$url"
  tar xzf "$DIR/.ig/$n/package.tgz" -C "$DIR/.ig/$n"
}
# copy <glob-prefix> <n> <dest>
copy() {
  local prefix="$1" n="$2" dest="$3"
  compgen -G "$DIR/.ig/$n/package/$prefix-*.json" > /dev/null || return 0
  cp "$DIR"/.ig/"$n"/package/"$prefix"-*.json "$DIR/$dest/"
}

i=0
for url in "${PACKAGES[@]}"; do
  i=$((i + 1)); fetch "$url" "$i"
  copy StructureDefinition "$i" profiles
  copy SearchParameter "$i" searchparameters
  if [[ "$TERMINOLOGY" == 1 ]]; then
    copy CodeSystem "$i" terminology
    copy ValueSet "$i" terminology
  fi
done
for url in "${TERM_PACKAGES[@]}"; do
  i=$((i + 1)); fetch "$url" "t$i"
  copy CodeSystem "t$i" terminology
  copy ValueSet "t$i" terminology
done

count() { ls "$DIR/$1" 2>/dev/null | grep -c '\.json$' || true; }
echo
echo "profiles/:          $(count profiles) StructureDefinitions"
echo "searchparameters/:  $(count searchparameters) SearchParameters"
[[ "$TERMINOLOGY" == 1 ]] && echo "terminology/:       $(count terminology) CodeSystems / ValueSets"
echo
echo "Next: start sazare from $DIR (sazare-server start). Existing data is"
echo "reindexed automatically on that start, so the new search parameters cover it."
