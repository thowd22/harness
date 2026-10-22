#!/bin/sh
# Prints the release notes for VERSION: the body of its CHANGELOG section
# (`## [X.Y.Z] - DATE` or `## X.Y.Z`), up to the next `## ` heading, with
# leading and trailing blank lines trimmed. Fails when the section is
# missing or empty, so a release never goes out without notes.
#
# usage: scripts/release-notes.sh VERSION|vVERSION [CHANGELOG]   (default CHANGELOG.md)
set -eu
# shellcheck source=scripts/release-common.sh
. "$(dirname "$0")/release-common.sh"
[ $# -ge 1 ] || die "usage: $0 VERSION [CHANGELOG]"
version=$(strip_v "$1")
changelog=${2:-CHANGELOG.md}
valid_version "$version" || die "not a version: $1 (want X.Y.Z)"
[ -f "$changelog" ] || die "no $changelog"

notes=$(awk -v v="$version" '
  function is_target(line,   h) {
    h = line; sub(/^## +/, "", h); sub(/[ \t]+-[ \t].*$/, "", h); sub(/[ \t]+$/, "", h)
    return h == v || h == "[" v "]" || h == "v" v || h == "[v" v "]"
  }
  /^## / { if (found) exit; if (is_target($0)) { found = 1; next } }
  found { lines[++n] = $0 }
  END {
    s = 1; while (s <= n && lines[s] ~ /^[ \t]*$/) s++
    e = n; while (e >= s && lines[e] ~ /^[ \t]*$/) e--
    for (i = s; i <= e; i++) print lines[i]
  }
' "$changelog")
[ -n "$notes" ] || die "$changelog has no notes for $version: add a '## [$version] - YYYY-MM-DD' section (scripts/release-bump.sh does this from '## Unreleased')"
printf '%s\n' "$notes"
