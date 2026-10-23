---
id: TASK-141
title: Fuzzy ranked slash-command completion
status: In Progress
assignee:
  - '@claude'
created_date: '2026-10-23 02:31'
updated_date: '2026-10-23 03:24'
labels:
  - tui
  - commands
  - interaction
milestone: m-3
dependencies:
  - TASK-35
  - TASK-36
documentation:
  - backlog/docs/doc-7 - Visual-Design-Dracula-Theme-Indicators-Tool-Cards.md
priority: medium
type: enhancement
ordinal: 141000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Typing / opens a completion dropdown, but it only matches commands whose name starts with or contains the typed text, sorted alphabetically: /mdl finds nothing. Users expect oh-my-pi-style fuzzy search. The Ctrl+P palette already uses the nucleo fuzzy matcher, so the dropdown should share it.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The slash dropdown uses the same nucleo matcher as the palette: gaps are allowed (/mdl matches /model) and results are ranked by match score, with exact and prefix matches first
- [x] #2 Matched characters are highlighted with theme tokens
- [x] #3 Descriptions are searched too, ranked below name matches, so /review finds a workflow whose description mentions review
- [x] #4 Namespaced commands (global:name, project:name) match on the bare name and on the full name
- [x] #5 Ranking is stable for equal scores and updates within one frame per keystroke with 1000 commands
- [x] #6 Arrow keys, Tab, Enter, Esc and clicks keep working as today, and tests cover gapped, prefix, description and namespaced matches
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
1. Add crates/harness-tui/src/fuzzy.rs: shared nucleo matcher wrapper (Fuzzy), query/fuzzy/substring pattern builders and a run-grouping highlight() span helper; switch the palette to it.
2. input.rs: cache per-command match keys (Utf32 full name + description, lowercase full/bare, namespace length) and alphabetical order in set_commands; rank by tier (exact > prefix > gapped name > description substring) then score then alphabetical; narrow candidates when the token only grows; reset selection when the token changes.
3. Highlight matched characters (Highlight token, bold) in the visible rows only; render_completion takes &mut self.
4. Tests for gapped, prefix/exact, description, namespaced, stable ties, highlight cells, click, keys, narrowing equivalence and 1000-command frame budget; validate in a real pty (tmux + ui-demo); docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented shared fuzzy module (crates/harness-tui/src/fuzzy.rs) used by the palette and the slash dropdown. Decisions: name matching is gapped fuzzy (AtomKind::Fuzzy, no palette operators so ! ^ $ ' in a command token are literal); description matching is a contiguous substring (AtomKind::Substring) so short tokens do not pull in scattered-letter description noise; namespaced names are scored once on the full name (a bare-name fuzzy match is always also a full-name match) and exact/prefix tiers compare the bare name directly; the bare name is highlighted when the typed text is a bare prefix. Perf: first version scored full+bare names and took ~21 ms/keystroke under heavy parallel load (load avg 40 on 16 cores from other lanes); now ~7 ms/frame in a debug build when idle (rank <= ~5 ms); per-step timings are printed if the budget assertion fails. PTY validation: tmux on a private socket running target/debug/harness ui-demo with temp HOME/XDG dirs at 120x36: /mdl -> /model with m,d,l bold #f1fa8c; /re -> prefix matches (remote, researcher, resume, reviewer) then gapped names; /session -> description matches with 'session' highlighted; /mo Down Tab -> '/model '; Esc closes; Enter on exact /new runs it. Checks: cargo nextest -p harness-tui (312 pass, repeated runs), -p harness (106 pass), clippy -p harness-tui --all-targets -D warnings clean, rustfmt --check on touched files. Note: I ran 'cargo fmt --all' once early on, which may have reformatted other lanes' in-progress files; afterwards only rustfmt on my own files.

Review fix (palette title highlighting): confirmed the regression. Old palette code kept the positions nucleo's Pattern::indices had pushed before the first failing word; the shared Fuzzy::indices clears on None, so a query like 'mod sw' on title 'Model' (desc 'Switch the model') bolded nothing. Added Fuzzy::word_indices (crates/harness-tui/src/fuzzy.rs): matches each word separately, keeps the positions of every word that matches the title, drops any partial positions from a word that fails, sorts and dedups. The palette now uses it. Small deliberate difference from the old code: words after a failing word are still highlighted ('sw mod' also bolds 'Mod'), so the result does not depend on word order. Fuzzy::indices is unchanged for the slash dropdown, which only highlights a field that matched. New tests: fuzzy::tests::word_indices_keep_words_that_match and palette::tests::title_highlights_words_that_match_it_when_others_match_elsewhere (checks Mod is bold in the Accent fg, 'el' is not; it fails against the old indices call). Checks: cargo nextest -p harness-tui 314/314 pass, clippy -p harness-tui --all-targets -D warnings clean, rustfmt --check on fuzzy.rs and palette.rs clean.

Integration audit (2026-10-23): every AC is backed by local evidence: full workspace `cargo nextest run --workspace --locked --profile ci` 1452 passed / 8 skipped with all lanes merged, plus clippy --workspace --all-targets -D warnings, fmt --check, doc tests and cargo deny all clean. DoD#1 also requires the tests to pass in CI, and nothing containing this change has run on GitHub yet (it is uncommitted), so DoD#1 is unchecked and the task is back In Progress. After the push, check DoD#1 once CI is green and set Done; TASK-142 is waiting on the same thing.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Slash completion now uses the palette's nucleo matcher through a new shared module crates/harness-tui/src/fuzzy.rs (palette refactored onto it). Ranking: exact name, then prefix (full or bare namespaced name), then gapped name matches by score, then description substring matches; ties stay alphabetical. Matched characters are highlighted with the highlight token (bold). Match keys are cached per command and typing forward narrows candidates, keeping 1000 commands well inside a 16 ms frame. Verified by new unit tests in input.rs and fuzzy.rs (gapped, prefix/exact, description, namespaced, stable ties, highlight cells, click/Tab/Enter/Esc/arrows, narrowing equivalence, 1000-command frame budget), the full harness-tui and harness suites, clippy, and a real pty session of harness ui-demo driven through tmux. Docs: docs/tui-views.md, docs/guide/palette.md, doc-7 token table.
<!-- SECTION:FINAL_SUMMARY:END -->
