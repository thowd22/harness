#!/bin/sh
# Runs `CMD... --version` and checks it prints exactly
#   harness VERSION (COMMIT)
# where COMMIT is the first 12 characters of the given commit SHA.
# CMD may include a wrapper, e.g. `qemu-aarch64 target/.../harness`.
#
# usage: scripts/release-smoke.sh VERSION COMMIT CMD...
set -eu
# shellcheck source=scripts/release-common.sh
. "$(dirname "$0")/release-common.sh"
[ $# -ge 3 ] || die "usage: $0 VERSION COMMIT CMD..."
version=$(strip_v "$1")
commit=$(printf '%s' "$2" | cut -c1-12)
shift 2
want="harness $version ($commit)"
got=$("$@" --version) || die "'$* --version' failed"
[ "$got" = "$want" ] || die "'$* --version' printed '$got', want '$want'"
echo "ok: $got"
