#!/bin/sh
# Harness overhead guard: runs `harness bench --fake` (offline, the fake
# provider, so only harness-side time is measured) and fails when the
# harness overhead per model call or the session start time regresses past
# a threshold. See docs/performance.md ("CI overhead guard").
#
# usage: scripts/bench-guard.sh [HARNESS_BIN]
#
# HARNESS_BIN defaults to target/debug/harness (CI builds the dev profile,
# the same binary the test step uses). Noise handling: each attempt runs
# BENCH_GUARD_RUNS sessions in one process and takes the best (minimum)
# value of each metric over them; an attempt over a threshold is retried, up
# to BENCH_GUARD_ATTEMPTS attempts, and the guard fails only when every
# attempt is over. Thresholds (milliseconds, dev profile):
#
#   BENCH_GUARD_MAX_OVERHEAD_MS       overhead per call (per-run p50, best run)
#   BENCH_GUARD_MAX_SESSION_START_MS  session start (best run)
#
# The run uses a fresh temporary HOME, XDG dirs and TMPDIR, so it never
# reads or writes the user's config or data. Needs only POSIX tools.
set -eu

# Thresholds: dev-profile baseline is ~3.3 ms overhead per call and ~11 ms
# session start (Linux x86_64, 16 CPUs, 2026-10-23); the limits leave ~4x
# headroom for slower, shared CI runners. Update docs/performance.md with
# any change here.
max_overhead=${BENCH_GUARD_MAX_OVERHEAD_MS:-15}
max_start=${BENCH_GUARD_MAX_SESSION_START_MS:-50}
runs=${BENCH_GUARD_RUNS:-7}
attempts=${BENCH_GUARD_ATTEMPTS:-3}

root=$(cd "$(dirname "$0")/.." && pwd)
bin=${1:-$root/target/debug/harness}
# measure() runs the binary from inside a temp dir, so a relative path (CI
# passes target/debug/harness) is made absolute against the caller's cwd.
case $bin in
    /*) ;;
    */*) if dir=$(cd "$(dirname "$bin")" 2>/dev/null && pwd); then
             bin=$dir/$(basename "$bin")
         else
             bin=$PWD/$bin
         fi ;;
    *) bin=$PWD/$bin ;;
esac
if [ ! -x "$bin" ]; then
    echo "bench-guard: $bin not found or not executable (build it with: cargo build -p harness)" >&2
    exit 2
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/harness-bench-guard.XXXXXX")
trap 'rm -rf "$work"' EXIT INT TERM
for d in home config data cache state tmp; do
    mkdir -p "$work/$d"
done

# Prints "<overhead> <session start>" for one attempt, or fails.
measure() {
    out="$work/bench-$1.json"
    (cd "$work" && env -u HARNESS_CONFIG -u HARNESS_DATA_DIR -u HARNESS_LOG \
        HOME="$work/home" \
        XDG_CONFIG_HOME="$work/config" \
        XDG_DATA_HOME="$work/data" \
        XDG_CACHE_HOME="$work/cache" \
        XDG_STATE_HOME="$work/state" \
        TMPDIR="$work/tmp" \
        "$bin" bench --fake --runs "$runs" --no-render --json "$out" >/dev/null) || {
        echo "bench-guard: harness bench --fake failed" >&2
        return 1
    }
    # The report's summary stats are flat objects ({"n":..,"min":..}), so
    # collapsing whitespace and matching up to the next `}` finds each min.
    flat=$(tr -d ' \t\r\n' <"$out")
    o=$(printf '%s' "$flat" | sed -n -E 's/.*"overhead_per_call_ms":\{[^}]*"min":(-?[0-9.eE+-]+).*/\1/p')
    s=$(printf '%s' "$flat" | sed -n -E 's/.*"session_start_ms":\{[^}]*"min":(-?[0-9.eE+-]+).*/\1/p')
    if [ -z "$o" ] || [ -z "$s" ]; then
        echo "bench-guard: $out has no summary.overhead_per_call_ms.min or summary.session_start_ms.min" >&2
        return 1
    fi
    echo "$o $s"
}

i=1
while [ "$i" -le "$attempts" ]; do
    result=$(measure "$i") || exit 1
    # shellcheck disable=SC2086 # two numbers, split on purpose
    set -- $result
    overhead=$1
    start=$2
    verdict=$(awk -v o="$overhead" -v s="$start" -v mo="$max_overhead" -v ms="$max_start" \
        'BEGIN { print ((o <= mo && s <= ms) ? "ok" : "over") }')
    printf 'bench-guard: attempt %s/%s (best of %s runs): overhead per call %.3f ms (max %s), session start %.3f ms (max %s): %s\n' \
        "$i" "$attempts" "$runs" "$overhead" "$max_overhead" "$start" "$max_start" "$verdict"
    if [ "$verdict" = ok ]; then
        echo "bench-guard: ok"
        exit 0
    fi
    i=$((i + 1))
done
echo "bench-guard: harness overhead regressed past the threshold in all $attempts attempts" >&2
echo "bench-guard: profile with 'harness bench --fake --runs 10'; if the increase is intended, raise the limits in scripts/bench-guard.sh and docs/performance.md" >&2
exit 1
