---
id: TASK-144
title: 'Human-readable names and descriptions for agents, runs and sessions'
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 04:24'
updated_date: '2026-10-23 06:44'
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
- [x] #1 Every agent has a display name and a one-line description (from its YAML, with sensible built-in defaults for coder, researcher and reviewer) used everywhere an agent is shown
- [x] #2 Every run gets a short human-readable title derived from its first prompt or workflow inputs, plus its agent or workflow, start time and status
- [x] #3 Palette run entries read like 'coder · fix the header parser · 14:32 Oct 23 · ✔' with the description summarising the outcome; the UUID is not shown, except as a short suffix when two titles collide
- [x] #4 The map header, run history, node detail, subagent cards and harness runs show use the same names; harness runs show still accepts full ids and unique id prefixes
- [x] #5 No raw UUID or hash is shown anywhere in the TUI by default; an audit test scans rendered screens of the UI gallery for UUID patterns
- [x] #6 Agent YAML gains optional display_name; the YAML spec and JSON schema are updated
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
1. harness-events::names: shared naming API (default/agent display names, title_from_text, short_tag, when_label, disambiguate, contains_raw_id) for runs now and sessions in the resume lane.
2. Agent YAML display_name (Agent::display_name()), built-in coder/researcher/reviewer display names + one-line descriptions, JSON schema + doc-3 + docs.
3. Events: RunStarted gains optional title and agent display-name map (serde default, skip when empty); reducer folds title (explicit, else first prompt) and names; RunState helpers for agent labels.
4. Emit: core.rs (session agents), workflow executor (title from inputs, names via with_agent_names, short tag in re-run/resume names), eval exec.
5. Views: palette run entries (agent · title · time · status, outcome description, short tag on collision), agent palette entries, map header + node boxes, detail header, history rows, goal bar owners, subagent cards (minimal), harness runs show (title header, short tag; still accepts full ids and prefixes, plus tags).
6. Gallery: add history page and past runs in the palette; audit test scans every page for UUID/hash patterns.
7. Tests for each AC, docs, CHANGELOG.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Shared API: harness_events::names (default_display_name, agent_display_name, title_from_text, title_from_inputs, shorten, scrub_ids, short_tag, matches_tag, when_label, status_glyph, disambiguate, collisions, find_raw_id) plus RunState::{label, who, node_label, node_agent, node_agent_detail, agent_display}. The resume lane should reuse title_from_text / when_label / short_tag / disambiguate for session names.
Events: run_started gains optional title and agent_names (serde default, omitted when empty, redacted); no schema version bump needed (additive optional fields). Reducer titles a run from run_started.title, else the first user_message of a non-workflow run.
Display names default to the name with -/_ as spaces and a capital first letter; built-ins set Coder/Researcher/Reviewer. Descriptions are unchanged so model requests and golden snapshots are unchanged; Defaults changelog entry added (fingerprint fb557d15ada9).
Tags are the last 6 hex digits of the run id (the random part of UUID v7; prefixes collide for runs created within a minute). harness runs show accepts full ids, prefixes, last and tags.
Palette: a kind with longer titles (runs) widens its own title column up to 3/5 of the row so time/status stay visible; builtins keep 2/5. Run labels cut titles to 24 chars.
Workflow re-run/resume run names now carry the short tag instead of the full origin uuid (executor test expectation updated accordingly, still asserting the reference and that the full id is absent).
Test expectations changed from lowercase agent names to display names (goalbar, map, subagent_cards, app subagent click, harness commands/ui_demo, history); snapshots reviewed and accepted: gallery pages (new history tab, palette runs, display names), map/detail/goalbar/runs_show_tree.
Gallery gains a History page and past runs in its palette; audit test no_page_shows_a_raw_id renders every page at 80x24, 120x32, 200x50.
TUI 'open run' panel now titled 'Run #tag' without the id line.
Validation: cargo nextest run --workspace (1497 passed), clippy -D warnings on touched crates, rustfmt on touched files, doc tests, docs_examples.

Review fixes (all 5 findings verified as real and fixed):
1. AC1: CommandRegistry agent commands (the real palette /coder etc.) now read 'Switch to <display_name>: <summary>' (registry.rs); /agents lists '<display_name>  <summary>  (/<name> · <origin>)'. Tests: harness-agent commands::tests::agent_commands_describe_agents_by_display_name and harness tui::tests::agents_show_by_display_name_in_the_palette_and_the_agents_panel (custom display_name agent in a trusted project, through palette_entries and /agents). palette::catalog_entries is still only used by its tests; kept (removing it would delete existing tests) - flagged for follow-up.
2/3. AC3: palette run labels are now 'Coder · title #tag · 14:32 Oct 23 · ✔'; PaletteEntry::keep_tail keeps tag, time and status and the palette cuts the title head instead (fit_title). History cuts what() before appending the tag. Tests at 80x24: palette::narrow_palettes_keep_the_tag_time_and_status_of_runs, gallery::colliding_runs_stay_distinct_with_status_at_80_columns.
4. Empty-query palette keeps run insertion order (newest first); test past_runs_stay_newest_first_with_an_empty_query. Gallery palette snapshot reviewed and accepted (runs newest first, tag before time).
5. Map header measures status/metrics first and gives the run name the leftover width (min 12); test map::tests::a_long_title_is_cut_before_the_header_metrics_at_80_columns.
Docs (tui-views.md, guide/palette.md) and CHANGELOG updated. Validation: cargo nextest run --workspace 1504 passed; clippy -D warnings on harness-tui/harness-agent/harness clean; cargo fmt.

Integration (2026-10-23): workspace green locally after all three lanes merged in the tree: cargo fmt --all --check clean; cargo clippy --workspace --all-targets --locked -D warnings clean; cargo nextest run --workspace --locked --profile ci 1538 passed, 8 skipped (pre-existing gated tests); cargo test --doc ok; cargo deny check ok; tests/install-test.sh ok; shellcheck on install.sh, scripts/*.sh, tests/*.sh clean. The earlier history.rs/palette.rs failures are green. No tests removed (55 #[test] added, 0 removed). DoD #1 left unchecked until CI passes on the pushed commit.

Open follow-up (not done here, needs a decision): palette::catalog_entries is only called by its own unit tests; either wire it into the app or remove it together with those tests.

Green in CI on ubuntu and macos (https://github.com/thowd22/harness/actions/runs/39911543976) and in the v0.2.0 release gate (https://github.com/thowd22/harness/actions/runs/39911979325); shipped in https://github.com/thowd22/harness/releases/tag/v0.2.0 and verified with the installed binary.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Agents, runs and (via the shared harness_events::names API) sessions now have human-readable names. Agent YAML gains optional display_name (built-ins Coder/Researcher/Reviewer; others default from the name) used in the palette, map, detail, goal bar, subagent cards, history and harness runs show. run_started records a title (workflow inputs) and agent display names; the reducer titles other runs after their first prompt. Palette runs read 'Coder · fix the header parser · 14:32 Oct 23 · ✔' with the outcome as description and a #tag only on collisions; harness runs show accepts ids, prefixes, last and tags. The UI gallery gains a History page and palette runs, and an audit test fails on any UUID/hash on any page. Verified with new unit/integration tests per AC (names, reducer, config, palette, history, show, map, workflow executor, harness TUI driver, gallery audit), reviewed snapshots, full workspace nextest (1497 passed) and clippy -D warnings.
<!-- SECTION:FINAL_SUMMARY:END -->
