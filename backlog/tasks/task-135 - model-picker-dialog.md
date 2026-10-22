---
id: TASK-135
title: /model picker dialog
status: Done
assignee:
  - '@claude'
created_date: '2026-10-22 19:47'
updated_date: '2026-10-22 21:30'
labels:
  - tui
  - provider
milestone: m-3
dependencies:
  - TASK-134
  - TASK-36
  - TASK-39
  - TASK-117
  - TASK-50
documentation:
  - backlog/docs/doc-7 - Visual-Design-Dracula-Theme-Indicators-Tool-Cards.md
  - backlog/docs/doc-5 - Agent-Map-and-Run-Visibility.md
priority: high
type: feature
ordinal: 135000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Typing /model with no argument currently only shows the current model in a toast. Users want a large picker that shows every usable model and its provider at a glance, without truncated or wrapped text.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 /model with no argument, and the palette action, opens a modal dialog whose height is 90 percent of the terminal height, centered
- [x] #2 Dialog width grows to fit the widest row so no row wraps; if that exceeds the terminal width it is capped at the terminal width minus margins and long cells are truncated with an ellipsis, never wrapped
- [x] #3 Rows show model, provider, alias, context window, price in and out, capabilities and auth method, in aligned columns grouped by provider
- [x] #4 The current model is marked, unusable models are dimmed with the reason, and typing filters rows with fuzzy matching
- [x] #5 Arrows, PgUp, PgDn, Home and End move the selection, Enter switches the model, Esc closes; rows are clickable and the wheel scrolls
- [x] #6 Opens in under 50 ms from the cached catalog and shows a refresh indicator while live lists update
- [x] #7 /model <name> still switches directly without opening the dialog
- [x] #8 Styled snapshot tests at several terminal sizes assert the 90 percent height and that no row wraps
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
1. New harness-tui module model_picker: ModelPicker over Arc<Catalog> (grouped rows with provider headers, aligned columns model/provider/alias/context/in/out/caps/auth/status, current marker, unusable rows dimmed with reason, nucleo fuzzy filter, keys Up/Down/Tab/PgUp/PgDn/Home/End/Enter/Esc, clickable rows and wheel via HitMap, refresh indicator from Catalog::refreshing). Dialog height = 90% of terminal height, centered; width = widest row, capped at terminal width minus margins with ellipsis-truncated cells.
2. App: CoreEvent::ModelPicker{catalog,current} opens the modal, CoreEvent::ModelCatalog updates it, UiRequest::SetModel(ref) on Enter/click; key, mouse and paste routing; render on top of everything.
3. Driver (crates/harness/src/tui): /model without args and new palette entry model.pick open the picker from a lazily created CatalogService (cached catalog, instant), spawn a background refresh and forward catalog updates; /model <name> unchanged.
4. Tests: unit + styled snapshots at several sizes (90% height, no wrap, truncation), key and mouse paths, open timing < 50 ms, driver tests for /model, palette entry and direct switch.
5. Docs: docs/commands.md, docs/models.md, docs/tui.md / guide palette; changelog.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_tui::model_picker (ModelPicker, PickerAction, PickerOutcome, dialog_height, PALETTE_ID, REFRESHING). App: CoreEvent::ModelPicker{catalog,current} / CoreEvent::ModelCatalog(Arc<Catalog>), UiRequest::SetModel(String); picker gets keys/mouse/paste first (after quit/interrupt/suspend/help) and renders over everything. Driver: lazily created CatalogService (wiring::model_catalog), forwarder task sends ModelCatalog on every watch update, spawn_refresh(false) after opening; palette entry model.pick; /model NAME unchanged. Column cutting is staged (status first, model id last) so ids stay readable. Choosing an unusable model keeps the dialog open with the reason (in-dialog notice, since toasts would be hidden under the modal). Snapshots reviewed by hand (cargo-insta not installed): picker_wide, picker_120x30, picker_80x24.
Validation: cargo test -p harness-tui (280 lib + integration, all pass), cargo test -p harness (all pass, incl. tui::tests::model_*), cargo clippy -p harness-tui -p harness --all-targets -D warnings clean, rustfmt --check on changed files clean. No live provider calls: driver tests use unset api_key_env for all providers.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added the /model picker: harness_tui::model_picker renders every catalog entry grouped by provider in aligned columns (model, provider, alias, context, in/out price, capabilities, auth, status) in a centered modal 90% of the terminal height, as wide as the widest row or capped at terminal width minus 2-cell margins with ellipsis-cut cells (never wraps). Current model marked, unusable rows dimmed with reason, fuzzy filter, arrows/Tab/PgUp/PgDn/Home/End/Enter/Esc, clickable rows, wheel, refresh indicator with in-place catalog updates. Wired through App (CoreEvent::ModelPicker/ModelCatalog, UiRequest::SetModel) and the TUI driver (/model without args and palette 'Switch model' open it from the cached CatalogService and refresh in the background; /model NAME switches directly). Verified with 18 tui tests (styled snapshots at 3 sizes plus layout checks at 6 more, keyboard and mouse paths, <50 ms open with 800+ models) and 2 driver tests; docs in docs/models.md, docs/commands.md, docs/tui.md, CHANGELOG.
<!-- SECTION:FINAL_SUMMARY:END -->
