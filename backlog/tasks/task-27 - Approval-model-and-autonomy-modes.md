---
id: TASK-27
title: Approval model and autonomy modes
status: Done
assignee:
  - '@policy'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - policy
  - problem-7
milestone: m-2
dependencies:
  - TASK-25
  - TASK-4
priority: high
type: feature
ordinal: 27000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Constant approval prompts cause fatigue while too much autonomy erodes trust (problem 7). Approvals need scopes and modes so the user decides once.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Modes ask, auto-edit and auto are selectable via ini, palette and per agent
- [x] #2 Approval scopes: once, this session for a command pattern, and always (writes a rule to harness.ini only after explicit confirmation)
- [x] #3 Parallel tool calls needing approval are batched into one prompt
- [x] #4 Approval requests and resolutions are events, so blocked nodes are visible in the map
- [x] #5 Number of approval prompts per run is recorded as a metric
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
1. approvals::mode: ModeState (ini default, palette override, per-agent tighten-only), palette_entries.
2. approvals::grants: SessionGrants (command patterns, path globs) shared with PolicyEngine.
3. approvals::broker: ApprovalBroker batches all ask calls of a turn into one ApprovalPrompt via ApprovalUi (ChannelUi for the TUI); scopes Once/Session/Always; Always yields IniRuleProposal written only on confirm (ini_edit::append_rule).
4. Events via ApprovalEvents (RecorderApprovalEvents -> approval_requested/resolved on the node); ApprovalMetrics + count_prompts.
5. Tests and docs/policy.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_policy::approvals (mode.rs, grants.rs, broker.rs, ini_edit.rs). Always-scope writes to [policy] bash.allow (outward actions are allowed by explicit bash.allow patterns, see TASK-28), atomic, keeps comments and 0600 mode. Missing answers / closed UI deny. Tests: approvals::broker::tests (6, incl. RunRecorder + RunState reducer showing WaitingApproval), ini_edit (3), mode (3), grants (1). REMAINING for AC#1: ini mode maps via From<harness_config Mode>; palette selection API exists (palette_entries, ModeState::apply_palette_command / cycle) but the TUI command palette must list the entries and call it; per-agent mode is supported by ModeState::effective(agent_mode) but agent YAML (harness_config::agents) has no mode key yet. Check AC#1 once both are wired with tests. DoD#4: this task adds no visible UI (approval prompt UI is TASK-38).

Integration: agent YAML gains `mode:` (tighten-only, schema enum); palette and /mode change the mode through Core::set_mode capped by ModeState::effective; subagents use CappedGate. Tests: tui tests mode_changes_reach_the_policy_gate, headless an_agent_mode_tightens_the_session_mode, dispatch capped gate tests. No new visible UI beyond palette entries (tested).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: agent YAML gains `mode:` (tighten-only, schema enum); palette and /mode change the mode through Core::set_mode capped by ModeState::effective; subagents use CappedGate. Tests: tui tests mode_changes_reach_the_policy_gate, headless an_agent_mode_tightens_the_session_mode, dispatch capped gate tests. No new visible UI beyond palette entries (tested). Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
