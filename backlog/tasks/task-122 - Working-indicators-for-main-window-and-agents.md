---
id: TASK-122
title: Working indicators for main window and agents
status: Done
assignee:
  - '@tui-views'
created_date: '2026-10-22 14:43'
updated_date: '2026-10-22 16:58'
labels:
  - tui
  - visibility
milestone: m-3
dependencies:
  - TASK-117
  - TASK-16
  - TASK-32
  - TASK-39
  - TASK-4
documentation:
  - backlog/docs/doc-7 - Visual-Design-Dracula-Theme-Indicators-Tool-Cards.md
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 122000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users need to see at a glance that something is happening, what it is, and for how long, both for the main agent and for every subagent. Silent stalls are a common source of confusion and wasted time.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 An activity line above the input shows a spinner, current phase, phase and turn elapsed time, live token counts, cost and the interrupt hint
- [x] #2 Phases cover sending, waiting for first token, thinking, writing, running a tool (with name and summary), retrying (reason and countdown), waiting for approval, verifying, compacting and syncing to a remote
- [x] #3 No provider data or tool output for ui stall_after seconds turns the indicator orange with the stall reason
- [x] #4 Running agents show spinners on map nodes, goal-bar tasks and subagent cards, and the status line shows counts of running agents and active remotes
- [x] #5 Optional terminal title shows spinner and status when ui terminal_title is true
- [x] #6 Spinners animate only while work is running and visible, redraw only affected cells, and ui animations = false switches to static icons
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
1. indicators::Spinner (static ◐ without animations), Phase enum covering all phases, Activity folded from main-agent RunEvents with phase/turn timers, tokens and cost. 2. Stall detection from last data time vs [ui] stall_after. 3. Activity line render; TerminalTitle OSC output. 4. node_glyph / running_agents / active_remotes helpers for map, goal bar, subagent cards and status line. 5. ChatView places the line above the input and exposes needs_tick(visible).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/indicators.rs and ChatView wiring. Tests: phases_follow_main_agent_events (all phases incl. retry countdown, compacting, syncing), subagent_events_do_not_change_the_main_phase, activity_line_shows_spinner_phase_times_tokens_cost_and_hint, stall_turns_the_indicator_orange_with_the_reason, spinner_static_without_animations_and_ticks_only_when_needed, spinner_tick_redraws_only_the_spinner_cell (Buffer::diff = 1 cell), node_glyphs_and_counts, terminal_title_only_when_enabled_and_changed, ChatView submit_shows_user_message_and_activity_line_above_input and terminal_title_follows_activity_when_enabled. Phases not in the event log (provider retry countdown, compacting, syncing) are set via ChatView::activity_mut().retrying/set_phase. Terminal title is emitted as ChatOutput::SetTitle(osc) for the app to write. REMAINING (AC#4): subagent cards (chat transcript) and status line counts are done, but spinners on map nodes and goal-bar tasks belong to the map/goal-bar modules (other owners): they should draw status icons with harness_tui::indicators::node_glyph(status, &spinner). Check AC#4 once those views use it.

AC4 closed by visibility-tui: map nodes and goal-bar tasks draw their status with indicators::node_glyph (map.rs draw_full/draw_line, goalbar::TaskStatus::glyph). Test: map::tests::running_nodes_and_goal_bar_tasks_spin (spinner frame advances on map nodes; goal-bar in-progress glyph == node_glyph(Running)); subagent cards and status-line counts were already tested.
<!-- SECTION:NOTES:END -->
