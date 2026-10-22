---
id: TASK-72
title: Change ledger
status: Done
assignee:
  - '@hygiene-verify'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - hygiene
  - problem-3
milestone: m-7
dependencies:
  - TASK-22
  - TASK-23
priority: high
type: feature
ordinal: 72000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Hygiene and review need a complete record of what changed, including changes made via bash.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Records every file created, modified or deleted per node, by tools directly
- [x] #2 Detects bash-made changes by comparing worktree state before and after each command
- [x] #3 Ledger is part of the event log and shown in node detail
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
1. harness_hygiene::snapshot: worktree snapshot (gitignore-aware walk, sha256, mtime/len cache, small-text cache) and diff of two snapshots.
2. harness_hygiene::ledger: Ledger records tool FileChange (created/modified) and bash-made changes (snapshot before/after each command, incl. deletions) per node, with net_changes per node.
3. Ledger emits every entry as EventKind::FileChanged through an EventSink (RunRecorder impl) and can be rebuilt from a run's events; node-detail rendering helper lines.
4. Tests: tool changes, bash create/modify/delete, event round trip. Docs docs/hygiene.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_hygiene::snapshot (gitignore-aware worktree snapshot with mtime/len hash cache and small-text cache, diff -> PathChange) and harness_hygiene::ledger (Ledger::new(root).with_sink(Arc<dyn EventSink>); record, record_tool_change(&harness_tools::tool::FileChange), begin_command/end_command for bash; entries/for_node/net_changes; from_events; summary_lines for node detail). Every entry is emitted as EventKind::FileChanged on its node (RunRecorder implements harness_hygiene::events::EventSink) and Ledger::from_events rebuilds it; the reducer's NodeState.files_changed counts them. Tests: cargo test -p harness-hygiene ledger snapshot (7 pass, incl. a real RunRecorder round trip). Docs: docs/hygiene.md#change-ledger. REMAINING (AC#3, DoD#4): node detail view (TASK-65, harness-tui) must render the node's changes (ledger::summary_lines(&Ledger::from_events(..).net_changes(Some(node))) or the file_changed events), with mouse/keyboard tests. Also harness-agent must route ToolEvent::FileChanged into Ledger::record_tool_change and wrap each bash call in begin_command/end_command, and not emit file_changed itself. The FileChanged event has no source field, so replayed entries have ChangeSource::Recorded.

Integration: LedgerBash wraps bash with begin_command/end_command; file changes reach the run log and node detail lists them with actions. Tests: headless files_changed_by_bash_reach_the_change_ledger, app node_detail_shows_the_ledger_and_hygiene_report_with_actions (mouse and keyboard).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: LedgerBash wraps bash with begin_command/end_command; file changes reach the run log and node detail lists them with actions. Tests: headless files_changed_by_bash_reach_the_change_ledger, app node_detail_shows_the_ledger_and_hygiene_report_with_actions (mouse and keyboard). Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
