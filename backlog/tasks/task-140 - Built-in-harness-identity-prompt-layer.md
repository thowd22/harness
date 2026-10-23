---
id: TASK-140
title: Built-in harness identity prompt layer
status: To Do
assignee: []
created_date: '2026-10-23 02:30'
updated_date: '2026-10-23 02:31'
labels:
  - prompts
  - problem-2
  - problem-6
milestone: m-4
dependencies:
  - TASK-43
  - TASK-49
  - TASK-111
  - TASK-115
references:
  - 'https://github.com/thowd22/harness'
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
priority: high
type: feature
ordinal: 140000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Models do not know what harness is. When a user asks about harness features (skills, workflows, agents, memory, the agent map, slash commands, harness.ini), the model guesses instead of looking up the real behaviour. A short identity layer compiled into the binary should name the key terms and point the model to the docs, so it reads them before answering. It must stay small because it is in every request (problem 6) and must be worded as guidance, not prohibitions (problem 2).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A built-in identity layer is defined in code (a const in harness-config, not a user-editable asset) and is the first layer of every built-in agent's system prompt
- [ ] #2 It names harness and its key terms (skills, workflows, agents, subagents, memory, dreaming, context, compaction, agent map, goal bar, slash commands, command palette, harness.ini, remotes) and tells the model that when the user mentions any of them it should consult https://github.com/thowd22/harness and its docs (docs/guide/ for usage, backlog/docs/ for design), preferring local docs/ when the repository is the working directory
- [ ] #3 The layer is at most 120 tokens, passes prompt lint with no warnings, and keeps the default agent within the initial-context CI budget
- [ ] #4 It sits in the stable prompt prefix, so prompt caching is unaffected across turns
- [ ] #5 general identity_prompt = on or off in harness.ini controls it (default on); custom prompts can include it explicitly as a layer named identity
- [ ] #6 Golden request snapshots and the CHANGELOG Defaults entry are updated, and /context shows the layer with its token count
- [ ] #7 An eval task checks that a question about a harness feature (for example how to add a workflow) leads the model to read the docs before answering
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
