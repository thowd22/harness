#!/bin/sh
# Prints the workspace version from Cargo.toml. With a TAG, also checks the
# tag is `v<that version>` and fails otherwise, so a tag can only release
# the version the tree actually builds.
#
# usage: scripts/release-version.sh [TAG]
set -eu
# shellcheck source=scripts/release-common.sh
. "$(dirname "$0")/release-common.sh"
version=$(workspace_version Cargo.toml)
valid_version "$version" || die "Cargo.toml [workspace.package] version '$version' is not X.Y.Z"
if [ $# -ge 1 ]; then
  [ "$1" = "v$version" ] || die "tag '$1' does not match the workspace version $version (want v$version); cut releases with the 'Cut release' workflow"
fi
printf '%s\n' "$version"
