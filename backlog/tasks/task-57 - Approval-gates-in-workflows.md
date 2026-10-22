---
id: TASK-57
title: Approval gates in workflows
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - workflow
  - problem-7
milestone: m-5
dependencies:
  - TASK-55
  - TASK-38
priority: medium
type: feature
ordinal: 57000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Plan-then-approve and ship-confirmation patterns need human checkpoints inside workflows.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 approval steps pause the workflow with a message and optional artifacts to review
- [x] #2 Waiting gates are highlighted in the map and reachable directly
- [x] #3 Headless mode resolves gates by flag
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
1. harness_workflow::gates: ApprovalGate trait, GateRequest (message, artifacts to review, node), GateDecision.
2. Executor approval steps record approval_requested/resolved on the node (the reducer shows WaitingApproval) and pause their branch.
3. HeadlessGates::from_flags(approve_all, approve, deny); ChannelGates for the TUI; waiting_gates(&RunState).
4. TUI highlights waiting gates and opens them directly.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Library side done in crates/harness-workflow/src/gates.rs (2 tests) plus executor integration (approval_gates_pause_with_message_and_artifacts: the message and artifacts reach the prompt, the node is WaitingApproval while dependents stay queued, waiting_gates finds it, headless approve/deny by flag). Remaining: AC#2 (harness-tui map owner) highlights nodes from gates::waiting_gates(&run_state) and opens the prompt with Enter or a click, answering the GatePrompt received from ChannelGates::new; needs mouse and keyboard tests (DoD#4). Binary owner: add --approve-all / --approve <step> / --deny <step> to the headless workflow command and build HeadlessGates::from_flags from them (the AC#3 library behaviour is tested). Validation: cargo test -p harness-workflow passes; clippy -D warnings clean.

Integration: workflow approval gates go to the chat approval queue (ChatGates); waiting gates are highlighted on the map and Enter/click jumps to the prompt (map blocked_nodes_are_highlighted_and_enter_jumps_to_the_prompt); headless --approve-all/--approve/--deny. Tests: tui workflow test, commands.rs wf test.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: workflow approval gates go to the chat approval queue (ChatGates); waiting gates are highlighted on the map and Enter/click jumps to the prompt (map blocked_nodes_are_highlighted_and_enter_jumps_to_the_prompt); headless --approve-all/--approve/--deny. Tests: tui workflow test, commands.rs wf test. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
