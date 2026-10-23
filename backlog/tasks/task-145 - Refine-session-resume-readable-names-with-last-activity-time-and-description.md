---
id: TASK-145
title: 'Refine session resume: readable names with last-activity time and description'
status: To Do
assignee: []
created_date: '2026-10-23 04:24'
updated_date: '2026-10-23 04:24'
labels:
  - tui
  - cli
  - agent
milestone: m-1
dependencies:
  - TASK-19
  - TASK-36
  - TASK-50
documentation:
  - backlog/docs/doc-5 - Agent-Map-and-Run-Visibility.md
priority: high
type: enhancement
ordinal: 145000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Resuming a session today means picking from a list of ids and first-prompt titles. Users need to recognise a session at a glance: a short name, when they last worked in it, and what it was about.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Each session has a short name (a few words, derived from the conversation, editable via a /rename command) and a one-line description summarising what it was about, updated as the session progresses
- [ ] #2 Session lists (/resume picker, harness sessions, palette) show name, the date and time of the last message (relative for recent ones, e.g. '14:32 today', 'yesterday 09:10', 'Oct 21 16:05'), project, model, message count and cost, newest activity first
- [ ] #3 The /resume picker is a fuzzy-searchable dialog over names and descriptions, fully clickable and keyboard-operable; harness run --resume accepts last, a name, or an id prefix
- [ ] #4 Generating names and descriptions uses no extra model call by default (derived locally); an optional cheap-model summary can be enabled in harness.ini
- [ ] #5 Tests cover name and description derivation, last-message timestamps and their formatting (including timezone and day boundaries), ordering, rename, fuzzy search and resume by name, plus a pseudo-terminal test of the picker
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
