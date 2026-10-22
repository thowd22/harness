---
id: TASK-95
title: Updated-memory card and undo
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - memory
  - tui
milestone: m-9
dependencies:
  - TASK-91
  - TASK-40
priority: medium
type: feature
ordinal: 95000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users must see and control what the agent remembers.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Each memory write shows an inline card listing changes
- [x] #2 Undo reverts the commit
- [x] #3 New memories apply to future sessions only
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
1. harness_memory::dream::history: history/recent_writes/diff/undo (git revert published through SessionCheckout::write).
2. harness_tui::memory::card::MemoryCard from the memory run event, with an undo button (mouse + keyboard).
3. Test that new memories reach only future sessions (frozen SessionIndex).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done: harness_tui::memory::card (MemoryCard::from_event(&RunEvent), parse_summary, render with [ undo ] registered in HitMap, mark_undone, MemoryCardAction::Undo(commit)); harness_memory::dream::history::undo(checkout, sha) reverts as one published commit. Tests: card tests (lists changes, says future sessions, undo by click and Tab+Enter, Esc -> Back), crates/harness-memory/tests/dream.rs memory_write_undo_reverts_the_commit and new_memories_apply_to_future_sessions_only. Docs: docs/memory-ui.md.
Remaining (AC#1, DoD#1): the card is a component; the chat transcript (harness-tui chat/transcript.rs, tui-views owner) still renders memory events as a notice. It must render MemoryCard for EventKind::Memory, route MemoryCardAction::Undo to the core, which calls history::undo and then mark_undone. harness-agent must emit EventKind::Memory{memory_id: change.commit.commit, summary: change.summary()} for each MemoryChange. Then add an integration test and check AC#1 and DoD#1.

Integration: MemoryAdapter emits EventKind::Memory with the commit sha; the transcript renders MemoryCard with undo routed to the core (history::undo). Tests: headless memory_writes_are_recorded_with_their_commit, tui memory_cards_undo_their_commit, transcript memory card tests.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: MemoryAdapter emits EventKind::Memory with the commit sha; the transcript renders MemoryCard with undo routed to the core (history::undo). Tests: headless memory_writes_are_recorded_with_their_commit, tui memory_cards_undo_their_commit, transcript memory card tests. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
