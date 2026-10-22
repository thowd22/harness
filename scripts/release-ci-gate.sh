#!/bin/sh
# Succeeds only when the CI workflow is green on commit SHA: at least one CI
# run for that commit concluded `success` and none concluded `failure`,
# `timed_out` or `action_required`. While runs for the commit are still
# queued or in progress it polls, up to RELEASE_CI_WAIT seconds (default
# 1800), every RELEASE_CI_POLL seconds (default 30).
#
# Needs `gh` authenticated (GH_TOKEN) and the repository in GITHUB_REPOSITORY
# (owner/name), both set on GitHub Actions runners.
#
# usage: scripts/release-ci-gate.sh SHA [WORKFLOW_FILE]   (default ci.yml)
set -eu
# shellcheck source=scripts/release-common.sh
. "$(dirname "$0")/release-common.sh"
[ $# -ge 1 ] || die "usage: $0 SHA [WORKFLOW_FILE]"
sha=$1
workflow=${2:-ci.yml}
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must be owner/name}
wait=${RELEASE_CI_WAIT:-1800}
poll=${RELEASE_CI_POLL:-30}
printf '%s\n' "$sha" | grep -Eq '^[0-9a-f]{7,40}$' || die "not a commit SHA: $sha"

waited=0
while :; do
  runs=$(gh api "repos/$repo/actions/workflows/$workflow/runs?head_sha=$sha&per_page=100" \
    --jq '.workflow_runs[] | "\(.status) \(.conclusion)"') || die "could not list $workflow runs for $sha"
  pending=$(printf '%s\n' "$runs" | awk 'NF && $1 != "completed"' | wc -l | tr -d ' ')
  if [ "$pending" -eq 0 ]; then
    bad=$(printf '%s\n' "$runs" | awk '$1 == "completed" && ($2 == "failure" || $2 == "timed_out" || $2 == "action_required")' | wc -l | tr -d ' ')
    good=$(printf '%s\n' "$runs" | awk '$1 == "completed" && $2 == "success"' | wc -l | tr -d ' ')
    [ "$bad" -eq 0 ] || die "$workflow failed on $sha; fix CI before releasing"
    [ "$good" -gt 0 ] || die "$workflow has no successful run on $sha; push the commit and wait for CI before releasing"
    echo "ok: $workflow is green on $sha ($good successful run(s))"
    exit 0
  fi
  [ "$waited" -lt "$wait" ] || die "$workflow still running on $sha after ${wait}s"
  echo "waiting for $pending $workflow run(s) on $sha..."
  sleep "$poll"
  waited=$((waited + poll))
  # A zero poll interval (tests) still has to make progress toward the limit.
  [ "$poll" -gt 0 ] || waited=$((waited + 1))
done
