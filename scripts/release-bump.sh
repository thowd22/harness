#!/bin/sh
# Prepares the tree for release VERSION:
#   * sets [workspace.package] version in Cargo.toml,
#   * updates the workspace members' versions in Cargo.lock (packages
#     without a `source`, i.e. path crates),
#   * turns CHANGELOG.md's `## Unreleased` section into
#     `## [VERSION] - DATE` and opens a fresh, empty `## Unreleased` above it.
# Run from the repository root. Changes nothing when any check fails.
#
# usage: scripts/release-bump.sh VERSION|vVERSION [DATE]   (DATE default: today, UTC)
set -eu
# shellcheck source=scripts/release-common.sh
. "$(dirname "$0")/release-common.sh"
[ $# -ge 1 ] || die "usage: $0 VERSION [DATE]"
new=$(strip_v "$1")
date=${2:-$(date -u +%Y-%m-%d)}
valid_version "$new" || die "not a version: $1 (want X.Y.Z or X.Y.Z-pre)"
printf '%s\n' "$date" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}$' || die "not a date: $date (want YYYY-MM-DD)"
for f in Cargo.toml Cargo.lock CHANGELOG.md; do [ -f "$f" ] || die "no $f (run from the repository root)"; done

old=$(workspace_version Cargo.toml)
[ -n "$old" ] || die "Cargo.toml has no [workspace.package] version"

# Refuse to go backwards (compare X.Y.Z numerically; pre-release tags are
# allowed to move freely within the same X.Y.Z).
core() { printf '%s\n' "${1%%-*}"; }
cmp=$(awk -v a="$(core "$new")" -v b="$(core "$old")" 'BEGIN {
  split(a, x, "."); split(b, y, ".")
  for (i = 1; i <= 3; i++) { if (x[i] + 0 > y[i] + 0) { print 1; exit } if (x[i] + 0 < y[i] + 0) { print -1; exit } }
  print 0 }')
[ "$cmp" -ge 0 ] || die "version $new is older than the current $old"

new_re=$(printf '%s' "$new" | sed 's/[.]/\\./g')
grep -Eq "^## \[?v?$new_re\]?([[:space:]]|$)" CHANGELOG.md && die "CHANGELOG.md already has a section for $new"
grep -q '^## Unreleased[[:space:]]*$' CHANGELOG.md || die "CHANGELOG.md has no '## Unreleased' section"
entries=$(awk '/^## / { if (u) exit; if ($0 ~ /^## Unreleased[ \t]*$/) u = 1; next } u && /^[ \t]*[-*] / { n++ } END { print n + 0 }' CHANGELOG.md)
[ "$entries" -gt 0 ] || die "the '## Unreleased' section of CHANGELOG.md has no entries; describe the release first"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

awk -v new="$new" '
  /^\[/ { in_pkg = ($0 == "[workspace.package]") }
  in_pkg && /^version[ \t]*=/ && !done { print "version = \"" new "\""; done = 1; next }
  { print }
' Cargo.toml > "$tmp/Cargo.toml"

awk -v old="$old" -v new="$new" '
  function flush(   i) {
    for (i = 1; i <= n; i++) {
      if (!has_source && lines[i] == "version = \"" old "\"") lines[i] = "version = \"" new "\""
      print lines[i]
    }
    n = 0; has_source = 0
  }
  /^\[\[package\]\]/ { flush() }
  /^source = / { has_source = 1 }
  { lines[++n] = $0 }
  END { flush() }
' Cargo.lock > "$tmp/Cargo.lock"

awk -v hdr="## [$new] - $date" '
  /^## Unreleased[ \t]*$/ && !done { print; print ""; print hdr; done = 1; next }
  { print }
' CHANGELOG.md > "$tmp/CHANGELOG.md"

[ "$(workspace_version "$tmp/Cargo.toml")" = "$new" ] || die "could not update Cargo.toml"
cp "$tmp/Cargo.toml" Cargo.toml
cp "$tmp/Cargo.lock" Cargo.lock
cp "$tmp/CHANGELOG.md" CHANGELOG.md
echo "bumped $old -> $new ($date)"
