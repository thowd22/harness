#!/bin/sh
# Packs FILEs into OUT_DIR/NAME.tar.gz (a top-level NAME/ directory holding
# the files) and writes OUT_DIR/NAME.tar.gz.sha256 in `sha256sum -c` format.
# Executable bits are kept.
#
# usage: scripts/release-package.sh OUT_DIR NAME FILE...
set -eu
# shellcheck source=scripts/release-common.sh
. "$(dirname "$0")/release-common.sh"
[ $# -ge 3 ] || die "usage: $0 OUT_DIR NAME FILE..."
out=$1 name=$2
shift 2
case $name in */* | "" | .*) die "bad archive name: $name" ;; esac
mkdir -p "$out"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/$name"
for f in "$@"; do
  [ -f "$f" ] || die "no such file: $f"
  cp -p "$f" "$stage/$name/"
done
archive="$out/$name.tar.gz"
( cd "$stage" && tar -cf - "$name" ) | gzip -9 > "$archive"
write_sha256 "$archive"
echo "packed $archive"
