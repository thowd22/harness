---
id: TASK-161.5
title: Opt-in background compaction
status: Done
assignee:
  - '@claude-bg-compaction'
created_date: '2026-10-23 21:08'
updated_date: '2026-10-23 22:49'
labels:
  - performance
dependencies: []
parent_task_id: TASK-161
priority: high
ordinal: 173000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Compaction runs in-line when context reaches compact_at = 0.8 and can block a turn for 10-60 s. Add [context] background_compaction (off by default): start the handoff summary in the background at a lower threshold (e.g. 0.7), keep working, swap it in at 0.8 together with the turns added since, then optionally pre-warm. Keep keep_recent_turns semantics and session resume exact.

Context: TASK-161 notes (baseline report, web research) and docs/performance.md; measure with harness bench (--fake for overhead, live via OpenRouter Opus 5.5, targeted and budget-capped).
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 With the option on, a long fake session crosses 0.8 without a blocking summary call and the resulting history matches what in-line compaction would keep (tests)
- [x] #2 Cancel, resume and failure paths are safe (summary failure falls back to in-line compaction); bench shows the blocking time removed
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [x] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [x] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [x] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Config: [context] background_compaction (bool, default false) and background_compact_at (fraction, default 0.7), both left out of the serialized config at their defaults (config hash unchanged); ContextSettings gains background_at: Option<f64> via from_config (core.rs and the eval runner pick it up with no wiring change).
2. handoff.rs: split Compactor::compact into start (cut, request, node_started + model_call_started events, synchronous), SummaryCall::run (the summary call, finish/cost/node events) and apply (swap the handoff in, keep messages[cut..]); compact() = start+run+apply, unchanged output and event order.
3. manager.rs (existing ContextManager request hook): with background_at set, when the estimate reaches background_at and no summary is pending, start the summary of messages[..cut] (cut from keep_recent_turns at that point), poll the call once inside the hook (request issued before the agent request; deterministic order with the scripted fake) and spawn the rest under a child of the run's cancel token. At compact_at: await the pending summary if still running (only the remaining time blocks), check the summarized prefix is unchanged (structural fingerprint that ignores elided results and dropped reasoning), swap it in with every message since the cut verbatim, charge its usage; on failure, cancel or a changed prefix fall back to in-line compaction. /compact with instructions and compact_now discard the pending summary. If the swapped history is still over compact_at, compact in-line. Drop cancels a pending summary.
4. Fake provider: optional per-turn match_system so a scripted summary reply serves the compaction request whatever its position (needed for an A/B bench with one script).
5. Bench: --compaction scenario (bundled script with a small window, a 2 s summary reply) and gap compaction_ms counted as the blocking part only (overlap with the context hook); before/after numbers with background_compaction off/on.
6. Tests (harness-agent tests/background_compaction.rs): long fake session crosses 0.8 with no blocking summary and matches in-line compaction (same handoff, in-line tail kept verbatim, identical when nothing was added), resume rebuilds exactly, summary failure falls back to in-line, cancel is safe and restarts, changed prefix falls back; config parse tests.
7. Docs: docs/context.md, docs/performance.md, harness.ini reference (doc-2 via backlog CLI + reference.ini fixture), CHANGELOG entry if the defaults fingerprint changes.

8. Review fixes: swap_background races the wait against the current turn's cancel token; ContextManager::stop_background called from Core::finish before run_finished; tests for both.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementation (2026-10-23, @claude-bg-compaction):
- Config: [context] background_compaction (bool, default false) and background_compact_at (fraction, default 0.7, validated 0<x<=1), both skipped in the serialized config at their defaults (config hash / defaults fingerprint unchanged; defaults_changelog test passes). ContextSettings.background_at via from_config, so core.rs and the eval runner pick it up with no wiring change. Test: harness-config background_compaction_is_off_by_default_and_hash_neutral.
- handoff.rs: Compactor::compact split into start (cut + node_started + model_call_started, synchronous), SummaryCall::run (call + finished/cost/node_finished) and apply (swap). In-line output and event order unchanged (node_finished moved before the swap, which emits nothing). CompactionReport.background marks a swapped-in background summary. CompactionError::Background + is_cancelled.
- manager.rs (existing ContextManager hook): below compact_at and at/over the background threshold, starts the summary of messages[..cut] (cut from keep_recent_turns at that point), polls it once inside the hook (request issued before the agent request: deterministic order) and spawns the rest under a child of the run's cancel token. At compact_at awaits it if still running, checks a structural fingerprint of messages[..cut] (roles, text, tool calls, result ids; ignores elided result content and reasoning), swaps it in keeping everything from the cut verbatim, charges its usage; if still over compact_at compacts in-line once more. Fallbacks to in-line: summary failure (no new background attempt until a compaction succeeds), cancellation, changed prefix. A summary cancelled with an earlier run is restarted. /compact and compact_now discard a pending summary (cancel + await so the node records cancelled). Drop cancels it.
- Fake provider: FakeTurn.match_system (scripted reply served to the request whose system prompt contains it, any position) so one bench script serves both settings. Test fake::tests::match_system_turns_serve_matching_requests_in_any_position.
- Bench: GapRow.compaction_ms now counts only the overlap of compaction calls with the context hook (the blocking part; unchanged for in-line); Summary.compaction_call_ms and compaction_blocking_ms; harness bench --fake --compaction (crates/harness-eval/bench/compaction.yaml: 20k window, 1.5 s agent calls, 2.5 s summary, compacts once with keep_recent_turns=6). Tests: bench::tests::a_background_compaction_counts_only_while_the_hook_waits, scenario test extended, harness bench::tests::compaction_variant_needs_the_fake_provider.
- Pre-warm after the swap not done here: it belongs to the cache pre-warm of TASK-161.4 (Anthropic max_tokens:0); nothing to pre-warm through the fake/OpenRouter path.

Tests (crates/harness-agent/tests/background_compaction.rs, gated summarizer + agent provider wrappers, no sleeps): AC#1 background_summary_runs_while_the_agent_works_and_matches_in_line_compaction (summary gated until agent call 8, so the run finishes only if the agent kept working; started before call 7 at 0.7, swapped before call 9 like in-line; identical requests before the swap, identical handoff, in-line tail is a suffix of the background tail, two more turns verbatim, resume rebuilds exactly, compaction node succeeded). AC#2: a_failed_background_summary_falls_back_to_in_line_compaction, a_background_summary_that_fails_early_is_not_retried_until_compact_at, cancelling_the_run_cancels_the_summary_and_the_next_run_restarts_it (node cancelled, history untouched, restart swaps in, resume exact), a_summary_of_a_history_that_changed_is_dropped, slash_compact_discards_a_pending_background_summary (resume exact). Unit: manager::tests (settings, fingerprint). 25/25 repeated runs green.

Bench before/after (dev profile, Linux x86_64, harness bench --fake --compaction --runs 1, interleaved A B A B; A defaults, B background_compaction = true): compaction call 2501.6/2501.9 ms vs 2502.3/2501.8 ms; compaction wait per run 2501.6/2501.9 ms vs 0.06/0.06 ms; turn 1 wall 22.15/22.15 s vs 19.65/19.68 s; run wall 31.25/31.26 s vs 28.75/28.79 s; summary request 5037 B vs 3769 B. Standard harness bench --fake --runs 5: overhead per call p50 3.32 ms (off) vs 3.86 ms (on, no compaction in that script; dev build, machine shared), 0 prefix breaks both.

Validation: cargo nextest -p harness-agent -p harness-eval -p harness-providers -p harness-config 677 passed (golden request snapshots and defaults_changelog included, no snapshot changes); -p harness 177 passed (harness::commands workflow_agent_steps_get_job_notices_and_their_jobs_end_with_the_run failed once under load, 3/3 alone and in the full rerun: background bash job timing, unrelated). clippy -D warnings clean for harness-agent/eval/providers/config and harness (--no-deps); rustfmt clean on my files (config.rs has rustfmt diffs in another task's anthropic tests, not mine). Live spend: $0 (a live check would need filling 70% of Opus' window; the fake bench isolates the blocking time). Docs: docs/context.md (Background compaction section + settings), docs/performance.md (Background compaction + numbers, --compaction), doc-2 harness.ini reference (via backlog doc update), reference.ini fixture, CHANGELOG Added.

Review fixes (2026-10-23, both blocking findings confirmed real):
1. A summary started in turn N runs under a child of turn N's token, and swap_background awaited it without watching turn N+1's token, so Esc in turn N+1 waiting at compact_at did nothing until the call ended. Fix (manager.rs swap_background): tokio::select! the wait against ctx.cancel.cancelled(); on cancel, cancel the summary's token and await its end (node recorded cancelled), return false, and the caller returns Continue (unchanged path). Test: harness-agent tests/background_compaction.rs a_later_run_cancelled_while_it_waits_for_an_earlier_summary_stops_at_once (run 1 ends normally with the gated summary pending, run 2 cancelled while blocked at compact_at; paused tokio clock, the cancel fires only once every task is idle, so no wall-clock timing; asserts Cancelled, run 1 token untouched, no compaction, node cancelled, history untouched, resume exact). Fails without the fix (timed out), passes with it.
2. Core::finish never stopped a pending background summary, leaving an open compaction node (node_started + model_call_started, no end) after recorder.finish. Fix: new pub async ContextManager::stop_background() (cancel + await, so the node records cancelled, or its finished result and Cost event when it had already completed); Core::finish calls it right after settle_summary, before run_finished and recorder.finish (one additive line in core.rs). Tests: harness-agent stop_background_records_the_end_of_a_pending_summary; harness tests/background_compaction.rs a_summary_pending_at_the_end_of_the_session_is_recorded_before_the_log_closes (real binary, fake script, 10k window, summary delay 600 s; asserts the compaction node has node_finished cancelled and model_call_finished, both before run_finished). Fails without the Core::finish call, passes with it.
Docs: docs/context.md background compaction bullets. Validation: cargo nextest -p harness-agent -p harness 358 passed; new agent tests 15/15 repeated runs green; clippy -p harness-agent -p harness --all-targets --no-deps -D warnings clean; rustfmt clean on touched files. No config/prompt changes (no CHANGELOG Defaults entry). Live spend $0.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added opt-in background compaction ([context] background_compaction, background_compact_at = 0.7; off by default, config hash unchanged). The existing ContextManager hook starts the handoff summary at 0.7 on its own task, keeps working, and swaps it in at compact_at with every message after its cut verbatim; failure, cancellation or a changed prefix fall back to in-line compaction, /compact discards a pending summary, resume stays exact. Review fixes: cancelling a later turn that waits at compact_at for a summary started earlier now stops it at once (select on the turn's token), and Core::finish stops a pending summary (ContextManager::stop_background) so the compaction node ends before the run log closes. Verified by 8 integration tests in harness-agent tests/background_compaction.rs, an end-to-end harness test (tests/background_compaction.rs), unit tests in manager/config/fake/bench, 358 crate tests passing, clippy clean; bench: compaction wait per run 2.5 s -> 0.06 ms. Docs: context.md, performance.md, doc-2, CHANGELOG. DoD#4 N/A. No live spend.
<!-- SECTION:FINAL_SUMMARY:END -->
