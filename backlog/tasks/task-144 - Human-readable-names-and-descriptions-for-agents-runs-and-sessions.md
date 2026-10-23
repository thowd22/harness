---
id: TASK-144
title: 'Human-readable names and descriptions for agents, runs and sessions'
status: To Do
assignee: []
created_date: '2026-10-23 04:24'
updated_date: '2026-10-23 04:24'
labels:
  - tui
  - visibility
  - config
milestone: m-6
dependencies:
  - TASK-67
  - TASK-36
  - TASK-44
documentation:
  - backlog/docs/doc-5 - Agent-Map-and-Run-Visibility.md
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
priority: high
type: enhancement
ordinal: 144000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The palette lists every past run as 'Run agent coder' with the description 'Open run 01a166f1-…', so runs are indistinguishable and identified only by UUIDs. Throughout the app (palette, map, history, detail, goal bar, subagent cards, CLI output) agents, runs and sessions need human-readable names and short descriptions; ids should be secondary, shortened and only shown where needed.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Every agent has a display name and a one-line description (from its YAML, with sensible built-in defaults for coder, researcher and reviewer) used everywhere an agent is shown
- [ ] #2 Every run gets a short human-readable title derived from its first prompt or workflow inputs, plus its agent or workflow, start time and status
- [ ] #3 Palette run entries read like 'coder · fix the header parser · 14:32 Oct 23 · ✔' with the description summarising the outcome; the UUID is not shown, except as a short suffix when two titles collide
- [ ] #4 The map header, run history, node detail, subagent cards and harness runs show use the same names; harness runs show still accepts full ids and unique id prefixes
- [ ] #5 No raw UUID or hash is shown anywhere in the TUI by default; an audit test scans rendered screens of the UI gallery for UUID patterns
- [ ] #6 Agent YAML gains optional display_name; the YAML spec and JSON schema are updated
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
