---
id: TASK-82
title: Token accounting
status: Done
assignee:
  - '@context-cost-commands'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:46'
labels:
  - context
  - problem-6
milestone: m-8
dependencies:
  - TASK-8
  - TASK-4
priority: high
type: feature
ordinal: 82000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Cost and context decisions need accurate per-call numbers.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Records provider-reported usage per call and aggregates per node, run, session and day
- [x] #2 Estimates request size before sending to drive compaction decisions
- [x] #3 Tracks context window usage per node
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [x] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [x] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. cost::UsageLedger folds model_call_*/cost events into CallRecords and groups per node/run/day/model/agent; sessions = from_runs(meta.runs); daily_spend reads all runs of a UTC day.
2. context::estimate: request estimates + ContextTracker anchored on provider-reported usage via the new RequestHook::after_call.
3. Per-node context use: ContextManager per loop + UsageLedger::context_by_node.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-agent/src/cost.rs, context/estimate.rs; ContextTracker is fed by the new agent_loop::hooks::RequestHook::after_call (CallReport). Zero-usage calls (fake provider) leave the tracker uncalibrated. Tests: cost::tests (4), context::tests (tracker/estimate), tests/budgets_and_done_gating.rs per_call_usage_cost_and_cache_hit_rate_are_recorded. Docs: docs/context.md, docs/cost.md.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Token accounting from the event log (UsageLedger: per call/node/run/session/day/model/agent, daily_spend, context_by_node) and pre-send request estimates (ContextTracker anchored on reported usage). Verified with cargo test -p harness-agent (cost, context and loop integration tests). No visible UI (DoD#4 N/A).
<!-- SECTION:FINAL_SUMMARY:END -->
