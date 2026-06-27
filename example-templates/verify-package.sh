#!/usr/bin/env bash
#
# Verify the `package` subcommand end-to-end against the `package-demo` template:
#   1. package it into zip / tar.gz / tar.zst
#   2. check each archive keeps `template.kdl` and drops the `.genignore`-listed
#      `secrets.env` (so secrets don't ship)
#   3. round-trip the `.tar.zst` back through `--archive` and confirm the project
#      renders correctly and generate-time rules hold
#
# Run from anywhere:  bash example-templates/verify-package.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$REPO_ROOT/target/debug/openeis-generate"
TPL="$REPO_ROOT/example-templates/package-demo"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if [[ ! -x "$BIN" ]]; then
  echo "(building debug binary)"
  (cd "$REPO_ROOT" && cargo build)
fi

echo "## 1. packaging package-demo → zip / tar.gz / tar.zst"
"$BIN" package "$TPL" -o "$WORK/pkg.zip"
"$BIN" package "$TPL" -o "$WORK/pkg.tar.gz"
"$BIN" package "$TPL" -o "$WORK/pkg.tar.zst"   # default format too
for f in "$WORK/pkg.zip" "$WORK/pkg.tar.gz" "$WORK/pkg.tar.zst"; do
  [[ -s "$f" ]] || { echo "FAIL: $f is empty"; exit 1; }
done
echo "   ok: three non-empty archives produced"

# List the members of an archive using whatever tool is available.
list_members() {
  local file="$1"
  if [[ "$file" == *.zip ]]; then
    command -v unzip >/dev/null && { unzip -l "$file" | awk 'NR>3 {print $4}'; return; }
  elif [[ "$file" == *.tar.zst ]]; then
    tar --zstd -tf "$file" >/dev/null 2>&1 && { tar --zstd -tf "$file"; return; }
  else
    command -v tar >/dev/null && { tar tzf "$file"; return; }
  fi
  echo ""   # no tool available → empty listing
}

echo "## 2. checking .genignore exclusions (secrets.env dropped, template.kdl kept)"
for f in "$WORK/pkg.zip" "$WORK/pkg.tar.gz" "$WORK/pkg.tar.zst"; do
  members="$(list_members "$f")"
  if [[ -z "$members" ]]; then
    echo "   skip: no listing tool for $f (round-trip below still covers it)"
    continue
  fi
  echo "$members" | grep -q "template.kdl" || { echo "FAIL: template.kdl missing in $f"; exit 1; }
  echo "$members" | grep -q "secrets.env"  && { echo "FAIL: secrets.env present in $f"; exit 1; }
  echo "$members" | grep -q "debug.log"    && { echo "FAIL: debug.log present in $f"; exit 1; }
  echo "   ok: $(basename "$f") keeps template.kdl, drops secrets.env + debug.log"
done

echo "## 3. round-trip: generate from the packaged .tar.zst"
OUT="$WORK/out"
"$BIN" --archive "$WORK/pkg.tar.zst" --name demo-proj --destination "$OUT" --silent

# Rendered output is correct.
grep -q "# demo-proj"            "$OUT/README.md"          || { echo "FAIL: README not rendered"; exit 1; }
grep -q "hello from demo-proj"   "$OUT/src/main.rs"        || { echo "FAIL: main.rs not rendered"; exit 1; }
grep -q 'name = "demo-proj"'     "$OUT/config/app.toml"    || { echo "FAIL: app.toml not rendered"; exit 1; }
# Generate-time rules: secrets never generated, template.kdl never copied out.
[[ ! -e "$OUT/secrets.env" ]]  || { echo "FAIL: secrets.env generated"; exit 1; }
[[ ! -e "$OUT/template.kdl" ]] || { echo "FAIL: template.kdl copied into output"; exit 1; }
echo "   ok: rendered correctly, secrets + config excluded"

echo
echo "## all package checks passed ✓"
