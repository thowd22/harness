---
id: TASK-52
title: Concurrent agent scheduler
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:33'
labels:
  - agent
  - subagents
milestone: m-5
dependencies:
  - TASK-51
priority: medium
type: feature
ordinal: 52000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Parallel subagents and workflow steps share provider rate limits and machine resources.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Global and per-provider concurrency limits are enforced
- [x] #2 Rate-limit responses slow the scheduler rather than failing runs
- [x] #3 Queued work is visible as queued nodes
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
1. harness_agent::scheduler: Scheduler with agent-slot semaphore (max_concurrent_agents) and per-provider call semaphores plus rate-limit pause deadlines.
2. ScheduledProvider wraps a Provider: takes a call permit per stream, pauses the provider on 429 retry events, and re-issues calls whose retries ran out on rate limits before any output.
3. acquire_agent(on_queued) lets callers record queued nodes; AgentSlot suspend/resume lends a parent's slot to its children.
4. Unit tests plus a subagent integration test for queued nodes; docs/subagents.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-agent/src/scheduler.rs (+ scheduler/tests.rs). Agent slots come from [general] max_concurrent_agents (SchedulerConfig::from_config). Per-provider limits are set in code (SchedulerConfig::with_provider_limit) because harness.ini has no key for them yet; a [provider.<name>] max_concurrent key is a follow-up for harness-config. Rate limits: note_rate_limited pauses new calls until retry-after (default 5s, capped 300s). ScheduledProvider retries whole calls exhausted by 429 before any content, up to 8 times, and emits a StreamEvent::Retry per wait. Queued nodes: subagents::tests::parallel_spawns_beyond_the_limit_are_queued_nodes shows node_started{queued:true} and a Queued node in the reducer. Validation: cargo test -p harness-agent --lib scheduler subagents (all pass). cargo clippy -p harness-agent -D warnings is clean for my modules; the only remaining failure is clippy::redundant_guards in crates/harness-agent/src/cost.rs, which another engineer owns.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added harness_agent::scheduler (Scheduler, AgentSlot, CallPermit, ScheduledProvider). It enforces global agent slots and per-provider call limits, pauses and re-issues on rate limits instead of failing, and reports queued work so callers record queued nodes. Verified with 8 scheduler unit tests plus the subagent queued-node integration test. DoD#4 N/A (no UI).
<!-- SECTION:FINAL_SUMMARY:END -->
