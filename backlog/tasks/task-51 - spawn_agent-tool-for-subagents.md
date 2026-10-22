---
id: TASK-51
title: spawn_agent tool for subagents
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:33'
labels:
  - agent
  - subagents
  - problem-6
  - problem-7
milestone: m-5
dependencies:
  - TASK-44
  - TASK-16
priority: high
type: feature
ordinal: 51000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Subagents must be user-controlled (which agent, which model) and cheap: a child gets a brief, not the parent's full context (problem 6).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 spawn_agent appears only for agents that list subagents and accepts only those names
- [x] #2 Child receives its own system prompt plus the brief and optional file list, not the parent transcript
- [x] #3 Depth and concurrency limits come from harness.ini
- [x] #4 Children appear as child nodes in the event stream and cancel with their parent
- [x] #5 Child returns a final summary and optional structured output validated against output_schema
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
1. harness_agent::subagents: SpawnAgentTool (agent enum = listed subagents, brief, files), ChildBuilder trait + CatalogChildBuilder over the Catalog.
2. SubagentRuntime::run_child: own system prompt + brief message, child node (kind subagent, parent id), scheduler slot (queued node), depth limit, cancel via child token and drop guard.
3. Structured output: extract the JSON, validate it against output_schema with robustness::validate, allow one correction turn.
4. SubagentLimits::from_config for [general] max_subagent_depth/max_concurrent_agents.
5. Tests with the fake provider; docs/subagents.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-agent/src/subagents.rs (+ subagents/tests.rs). The parent loop must run with the node id and cancel token given in AgentPlace (tools are bound to them because ToolContext carries no node or token). Each child runs in a tokio task, so a dropped parent tool call (cancellation) still lets the child record node_finished=cancelled. spawn_agent is read_only=true so parallel spawns run concurrently; children's own calls go through their gate. At the depth limit spawn_agent is not offered. Tests: offered only with subagents and only listed names; child request has own system + brief + files only; child node in reducer with Spawn edge; cancel with parent; ini limits; queued node; structured output valid / corrected / invalid. All pass (cargo test -p harness-agent --lib subagents).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added harness_agent::subagents (SpawnAgentTool, SubagentRuntime, ChildBuilder, CatalogChildBuilder, ChildRequest/ChildResult, brief_message, extract_json, check_output). Children get their own prompt plus a brief, run as child nodes under ini depth and concurrency limits, cancel with their parent, and return a summary plus validated structured output. Verified by 11 tests with the fake provider. DoD#4 N/A (no UI).
<!-- SECTION:FINAL_SUMMARY:END -->
