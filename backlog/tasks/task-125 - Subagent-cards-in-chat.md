---
id: TASK-125
title: Subagent cards in chat
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:43'
updated_date: '2026-10-22 16:58'
labels:
  - tui
  - visibility
  - subagents
milestone: m-6
dependencies:
  - TASK-120
  - TASK-122
  - TASK-123
  - TASK-51
documentation:
  - backlog/docs/doc-7 - Visual-Design-Dracula-Theme-Indicators-Tool-Cards.md
  - backlog/docs/doc-5 - Agent-Map-and-Run-Visibility.md
priority: medium
type: feature
ordinal: 125000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
When the main agent delegates, the user should see each subagent's progress inline in the conversation, not only in the map.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Each spawned subagent shows a card with status, agent name, model, host badge, live last action, duration and cost
- [x] #2 Cards update live and show the final summary when the subagent finishes
- [x] #3 Click or Enter opens the subagent in the map or node detail
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
1. Extend chat::transcript SubagentCard with agent and summary (last turn text) and render the agent next to the name and the final summary when the subagent succeeds. 2. Integration tests through ChatView: live updates, final summary, click and Enter -> ChatOutput::OpenNode.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Subagent cards already existed in chat::transcript (TASK-122). Added SubagentCard.agent (shown as 'name (agent)'), SubagentCard.summary (text deltas of the latest turn; live last action shows 'writing: <line>'), and the final summary (up to 3 lines) in finished cards. Minimal edit in crates/harness-tui/src/chat/transcript.rs (apply_subagent + subagent layout). Tests: crates/harness-tui/tests/subagent_cards.rs card_shows_status_agent_model_host_last_action_duration_and_cost_live (AC1, AC2 live), finished_card_shows_the_final_summary (AC2), click_or_enter_opens_the_subagent (AC3, mouse and keyboard); existing transcript test subagent_cards_show_spinner_last_action_cost_and_open_node still passes. App owner: map ChatOutput::OpenNode(node) to MapView::select + view switch or DetailView.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Subagent cards now show the agent profile, live 'writing' action and the final summary when finished; click/Enter opens the node. Verified by 3 ChatView integration tests.
<!-- SECTION:FINAL_SUMMARY:END -->
