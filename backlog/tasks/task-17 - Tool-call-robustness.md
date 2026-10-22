---
id: TASK-17
title: Tool-call robustness
status: Done
assignee:
  - '@agent-loop'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:15'
labels:
  - core
  - agent
  - problem-1
milestone: m-1
dependencies:
  - TASK-16
priority: high
type: feature
ordinal: 17000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The model's plan often fails at the tool boundary through bad JSON, wrong argument types, unknown tools or unhelpful errors. This is the largest bug hotspot in real harnesses (problem 1).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Tool arguments are validated against the tool schema before execution
- [x] #2 Lenient JSON repair fixes common defects such as trailing commas or truncated strings, and repairs are logged
- [x] #3 Errors returned to the model are short, actionable and say how to correct the call
- [x] #4 Unknown tools, duplicate call ids and missing results are handled without crashing the loop
- [x] #5 Read-only calls in one turn run in parallel while mutating calls run serially in order
- [x] #6 Repeated identical failing calls trigger a loop-detection event and a nudge
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
1. robustness: lenient JSON repair (fences, prose, trailing commas, truncated strings/containers, python literals, single quotes) returning the list of repairs; minimal JSON-schema validator + safe coercions; short actionable error formatter; LoopDetector for repeated identical failing calls.
2. dispatch: AgentTool trait + ToolSet registry with adapters for harness-tools read/write/edit/bash; prepare calls (dedupe ids, unknown tools, invalid args), ToolGate trait (AllowAll, PolicyGate), parallel read-only groups and serial mutating calls in order; ensure every tool_use gets a result.
3. Tests for each AC.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
robustness.rs: parse_arguments/repair_json (fences, prose, trailing commas, single quotes, raw newlines/tabs, Python literals, truncated strings, dangling ,/:, unclosed brackets, trailing text; every repair returned and logged on target harness::tools), validate() (type/properties/required/additionalProperties/enum/minimum/maximum/items with safe coercions logged as repairs), ToolErrorMessage ('Error in `tool`: problem. Fix: ...', capped at 1200 chars, did-you-mean hints), LoopDetector (3 identical failures -> nudge), ensure_tool_results (synthesizes missing results). dispatch.rs: AgentTool trait + adapters for read/write/edit/bash, ToolSet (standard/select/specs/bash_cwd), prepare_calls (renames duplicate ids to <id>__N and writes repaired args back into the assistant message), ToolGate (AllowAll, PolicyGate = PolicyEngine + ApprovalBroker, one batch per turn), Dispatcher (consecutive read-only calls via join_all, mutating calls serially in order, every call raced with the cancel token), RecorderToolEvents (file_changed events), RunBlobSink (spills into the run's blob store). Tests: robustness unit tests (16), dispatch unit tests (9, incl. barrier-based parallelism proof and in-flight<=1 for mutating calls), agent_loop integration tests for loop detection and malformed/unknown calls.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
JSON repair, schema validation with coercion, actionable errors, duplicate-id/unknown-tool/missing-result handling, parallel read-only + serial mutating dispatch and loop detection; verified by unit and fake-provider integration tests.
<!-- SECTION:FINAL_SUMMARY:END -->
