---
id: TASK-140
title: Built-in harness identity prompt layer
status: In Progress
assignee:
  - '@claude'
created_date: '2026-10-23 02:30'
updated_date: '2026-10-23 03:24'
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
- [x] #1 A built-in identity layer is defined in code (a const in harness-config, not a user-editable asset) and is the first layer of every built-in agent's system prompt
- [x] #2 It names harness and its key terms (skills, workflows, agents, subagents, memory, dreaming, context, compaction, agent map, goal bar, slash commands, command palette, harness.ini, remotes) and tells the model that when the user mentions any of them it should consult https://github.com/thowd22/harness and its docs (docs/guide/ for usage, backlog/docs/ for design), preferring local docs/ when the repository is the working directory
- [x] #3 The layer is at most 120 tokens, passes prompt lint with no warnings, and keeps the default agent within the initial-context CI budget
- [x] #4 It sits in the stable prompt prefix, so prompt caching is unaffected across turns
- [x] #5 general identity_prompt = on or off in harness.ini controls it (default on); custom prompts can include it explicitly as a layer named identity
- [x] #6 Golden request snapshots and the CHANGELOG Defaults entry are updated, and /context shows the layer with its token count
- [x] #7 An eval task checks that a question about a harness feature (for example how to add a workflow) leads the model to read the docs before answering
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [x] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [x] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [x] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. harness-config prompts: IDENTITY const + IDENTITY_LAYER name; new layer form `builtin: identity` (LayerSource::Builtin); Prompt.identity flag skips it when off; effective_layers() for render, lint and /context.
2. Built-in coder/researcher/reviewer prompts start with `- builtin: identity`; defaults fingerprint covers the const.
3. [general] identity_prompt (bool, default on); Catalog::set_identity_prompt applies it to every prompt and survives reload; wired in crates/harness (core, wiring::catalog callers).
4. /context: identity layer row with source 'built-in identity layer' and token count.
5. Eval: new `consulted` check (tool-call arguments mention a path) + identity suite with good/bad fake scripts and a test.
6. Golden snapshots via cargo insta review, CHANGELOG Defaults entry + fingerprint, docs (defaults, yaml-config, guide/yaml, guide/harness-ini, evals, context, doc-2, doc-3).
7. fmt, clippy, tests for touched crates, then workspace tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Design: the identity layer is a const (harness_config::prompts::IDENTITY, 114 estimated tokens) inserted by a new layer form `- builtin: identity` (LayerSource::Builtin). All three built-in prompts start with it. Prompt.identity (set for the whole catalog by Catalog::set_identity_prompt, re-applied on reload) skips it when [general] identity_prompt = off; wiring::catalog/catalog_from apply the setting for core, lint, eval and wf commands. identity_prompt accepts on/off (and true/false). /context splits the leading identity layer into its own 'identity layer' row with its token count (ContextReport::add_system_prompt); add_prompt labels builtin layers 'built-in identity layer'. Defaults fingerprint now covers the const. Eval: new `consulted:` check (tool-call arguments contain a string) and suite crates/harness-eval/suites/identity with good/bad fake scripts (tests/identity.rs).
Snapshots: cargo-insta is not installed, so the review was done by hand: INSTA_FORCE_PASS=1 INSTA_UPDATE=new wrote .snap.new files, a script verified each differs from the old snapshot only by the identity text plus separator (1x per prompt snapshot, 3x per request snapshot: anthropic, openai, openrouter), then they were accepted (assertion_line metadata stripped).
Validation: cargo nextest run --workspace 1439 passed; clippy -D warnings clean for harness-config, harness-eval, harness-tui, harness; cargo fmt --all --check clean; defaults_changelog passes with fingerprint 95e10042f5f1. Note: cargo fmt -p harness-tui/-p harness may have reformatted other lanes' files in those crates (formatting only).

Integration audit (2026-10-23): every AC is backed by local evidence: full workspace `cargo nextest run --workspace --locked --profile ci` 1452 passed / 8 skipped with all lanes merged, plus clippy --workspace --all-targets -D warnings, fmt --check, doc tests and cargo deny all clean. DoD#1 also requires the tests to pass in CI, and nothing containing this change has run on GitHub yet (it is uncommitted), so DoD#1 is unchecked and the task is back In Progress. After the push, check DoD#1 once CI is green and set Done; TASK-142 is waiting on the same thing.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added the built-in harness identity prompt layer: a ~114-token const in harness-config (prompts::IDENTITY) inserted with `- builtin: identity` as the first layer of the coder, researcher and reviewer prompts. It names harness and its key features and points the model to docs/guide/ and backlog/docs/ (locally in the harness repo, else https://github.com/thowd22/harness). [general] identity_prompt = on|off (default on) controls it via Catalog::set_identity_prompt; /context shows it as 'identity layer' with its token count. Golden snapshots updated (diff verified to be only the identity prefix), CHANGELOG Added + Defaults entry (fingerprint 95e10042f5f1), docs (defaults, yaml-config, guide/yaml, guide/harness-ini, context, visibility, evals, doc-2, doc-3), and a new identity eval suite with a `consulted:` check. Verified with unit tests in prompts/builtins/ini/discovery, wiring and TUI /context tests, golden_requests (incl. cached-prefix test), tests/identity.rs, prompt lint and the initial-context budget tests; full workspace nextest 1439 passed, clippy and fmt clean. DoD 4: no new interactive UI elements (the /context row reuses existing rows), so N/A.
<!-- SECTION:FINAL_SUMMARY:END -->
