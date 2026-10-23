#!/bin/sh
# Hermetic test guard: runs the harness and harness-eval test suites with
# HOME and every XDG base directory pointed at an empty sentinel directory,
# then fails if anything was created there. A test that writes to the
# user's real config, data or cache directory would write here instead.
#
# usage: scripts/hermetic-guard.sh [CARGO_ARGS...]
#
# With no arguments it runs `cargo nextest run -p harness -p harness-eval`
# (or `cargo test` when nextest is not installed). CARGO_ARGS replace that
# default, e.g. `scripts/hermetic-guard.sh test -p harness --test headless`.
# HARNESS_GUARD_CARGO names the cargo program (tests use a fake one).
set -eu

# Keep the real toolchain: resolve cargo and rustup homes before HOME moves.
CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}
export CARGO_HOME RUSTUP_HOME

cargo=${HARNESS_GUARD_CARGO:-cargo}
if [ $# -eq 0 ]; then
    if "$cargo" nextest --version >/dev/null 2>&1; then
        set -- nextest run -p harness -p harness-eval
    else
        set -- test -p harness -p harness-eval
    fi
fi

sentinel=$(mktemp -d "${TMPDIR:-/tmp}/harness-hermetic.XXXXXX")
trap 'rm -rf "$sentinel"' EXIT INT TERM
for d in home config data cache state; do
    mkdir -p "$sentinel/checked/$d"
done
# XDG_RUNTIME_DIR holds per-boot sockets (SSH ControlMaster), not user
# state: it is redirected too, so tests never touch the real one, but
# what appears there is not a leak.
mkdir -p "$sentinel/runtime"
chmod 700 "$sentinel/runtime"

status=0
env -u HARNESS_CONFIG -u HARNESS_DATA_DIR -u HARNESS_LOG \
    HOME="$sentinel/checked/home" \
    XDG_CONFIG_HOME="$sentinel/checked/config" \
    XDG_DATA_HOME="$sentinel/checked/data" \
    XDG_CACHE_HOME="$sentinel/checked/cache" \
    XDG_STATE_HOME="$sentinel/checked/state" \
    XDG_RUNTIME_DIR="$sentinel/runtime" \
    "$cargo" "$@" || status=$?

leaked=$(cd "$sentinel/checked" && find . -mindepth 2 | sort)
if [ -n "$leaked" ]; then
    echo "hermetic-guard: tests wrote to HOME or XDG directories:" >&2
    printf '%s\n' "$leaked" | sed 's|^\./|  |' >&2
    exit 1
fi
if [ "$status" -ne 0 ]; then
    echo "hermetic-guard: tests failed (exit $status)" >&2
    exit "$status"
fi
echo "hermetic-guard: ok, nothing written to HOME or XDG directories"
