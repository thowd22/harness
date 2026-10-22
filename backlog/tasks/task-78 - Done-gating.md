---
id: TASK-78
title: Done-gating
status: Done
assignee:
  - '@context-cost-commands'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - verify
  - problem-5
milestone: m-7
dependencies:
  - TASK-77
  - TASK-16
priority: high
type: feature
ordinal: 78000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Agents confidently declare success on broken work. The harness, not the model, decides whether work is verified.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 When an agent ends a turn with changes and verify is configured, the harness runs verify
- [x] #2 Failures are returned to the agent as feedback up to max_fix_attempts
- [x] #3 Final status is verified, unverified or failed and is shown honestly in chat, map and exit code
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
Minimal change in agent_loop: after Done(EndTurn) with a Verifier, run it (VerifyWhen::Always|OnChanges); on failure push a labeled, redacted feedback user message and continue up to LoopConfig::max_fix_attempts; LoopOutcome.verification = Verified|Unverified|Failed; failed => node_finished failed with verify_failed code (round-trips to EndTurn).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Defaults keep existing behaviour (verify_when=Always, max_fix_attempts=0) so the binary's tests still pass. Tests: tests/budgets_and_done_gating.rs verify_failures_are_fed_back_until_the_fix_passes, verification_that_keeps_failing_ends_failed_after_max_fix_attempts (node failed in events and reducer, exit-code path via verified=Some(false)), no_changes_means_unverified_when_verifying_only_on_changes. Open AC#3: exit code (3) and map node status are honest; chat must show LoopOutcome.verification (TUI), and the binary should set config.max_fix_attempts = cfg.verify.max_fix_attempts and verify_when = OnChanges. Docs: docs/done-gating.md.

Integration: verify runs with VerifyWhen::Always/OnChanges and cfg max_fix_attempts; final status verified/unverified/failed is shown in chat notices, map and exit code 3. Tests: headless verification test (4 failed verifies, exit 3), transcript verification notices.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: verify runs with VerifyWhen::Always/OnChanges and cfg max_fix_attempts; final status verified/unverified/failed is shown in chat notices, map and exit code 3. Tests: headless verification test (4 failed verifies, exit 3), transcript verification notices. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
