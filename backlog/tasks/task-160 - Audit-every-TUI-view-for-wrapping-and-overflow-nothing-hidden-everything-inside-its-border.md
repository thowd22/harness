---
id: TASK-160
title: >-
  Audit every TUI view for wrapping and overflow: nothing hidden, everything
  inside its border
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 19:49'
updated_date: '2026-10-23 21:11'
labels:
  - tui
  - layout
dependencies: []
priority: high
ordinal: 167000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
User requirement (2026-10-23): everything must fit on screen within its border regardless of terminal size or content length; nothing may be hidden from view. Today several places cut text with '…' or let it run under or past a border, for example the status line path ('/private/var/folders/n6/hn37q6pj7sj_9dm…'), session picker details ('…/T/.tmpX/project'), toast titles/bodies (truncate()), picker rows, map node labels, card headers, the activity line and footer hints.

Audit the whole TUI (crates/harness-tui and crates/harness/src/tui): chat transcript and tool/subagent cards (headers, bodies, expand hints), activity line, input box, status line and footer/tabs, goal bar and its collapsed indicator, agent map and node detail, diff view, command palette, /model and /resume pickers, login modal, /mcp and other panels, jobs view, approvals prompt, toasts and notification history, help overlay, keyboard selection bar, and any other popup. For each: long words, paths and URLs hard-wrap inside the border; multi-line content wraps rather than truncating; wide/CJK/emoji characters and tabs are measured correctly; nothing is drawn outside or over a border; and where space is genuinely too small (e.g. a 1-row status line), the full value stays reachable (wrapping onto more rows, a scrollable area, an expand/detail view, or a tooltip/toast on focus) and the UI says how to reach it. Popups size to their content within the screen and scroll when they cannot fit.

Add a layout test harness: render every view with long-content fixtures at a matrix of sizes (e.g. 40x12, 60x20, 80x24, 120x40, 250x60) on TestBackend and assert (a) no content cell lands on or outside a border, (b) every fixture token is visible or reachable through the documented action, (c) no panic at tiny sizes. Fix every finding.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 An inventory in the task notes lists every view and component checked, with its findings and fixes
- [x] #2 Long words, paths, URLs and wide characters wrap inside borders in every view; no text is drawn on or past a border at any tested size
- [x] #3 No information is hidden: anything shortened for space is reachable (wrap, scroll, expand or detail) and the UI hints how
- [x] #4 A layout test matrix renders every view with long-content fixtures at 40x12 to 250x60 and asserts the rules above; it runs in CI and stays fast
- [x] #5 Popups size to content within the screen and scroll when too large; mouse and keyboard work for any new scroll/expand affordance
- [x] #6 docs/tui.md states the layout rules
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
1. Inventory: render every view/component with tagged long-content fixtures (long words, deep paths, URLs, CJK/emoji, tabs, many items) at 40x12, 60x20, 80x24, 120x40, 250x60 (+ tiny sizes for panics) on TestBackend via a new test-only harness crates/harness-tui/src/layout_matrix.rs; record findings in the task notes before fixing.
2. Shared helpers in chat/text.rs: wrap_lines, wrap_text, take_wrap_marker (keeps the zero-width soft-wrap marker the FIRST span when chrome is prepended); sanitize expands tabs to 4-column stops.
3. Fix per area, preferring wrapping over truncation: transcript cards (header summary, body, compact details, subagent card, notices, meta line, markdown code/table), activity line, status line + footer + select bar (rows grow; full value reachable via a status details panel with a hint/button), toasts/notification history, approvals, login, help, panel (wrap + scroll), palette + slash completion, /model and /resume pickers, map/node detail/goal bar/live preview, diff, inspector/context, history, memory browser/card, ui-demo gallery tabs.
4. Popups size to content within the screen and scroll when too big; new scroll/expand affordances work by mouse and keyboard, with tests for both and semantic theme tokens.
5. Layout matrix asserts: no content on/past a border, every fixture token visible or reachable via the documented action, no panic at tiny sizes, no raw control chars; runs in the normal suite.
6. docs/tui.md layout rules, CHANGELOG Unreleased; fmt, clippy, nextest; verify in a private tmux server at 50x15 and a wide size.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Inventory, part 1 (chat, chrome; surveyed with the new layout matrix at 40x12, 60x20, 80x24, 120x40, 250x60 using tagged long-content fixtures):
- Layout matrix harness (new, test-only): crates/harness-tui/src/layout_matrix.rs. check_borders finds every box (corners) and fails when a side border is overdrawn, a box runs past the edge or misses a corner; shows() matches a token wrapped over rows (after borders/gutters/indents, with anything allowed after a part on its row, a cut part fails); assert_reachable over the frames a case collects while doing the documented action; matrix() runs each case at the 5 sizes plus 1x1, 8x3, 20x5, 30x8 for panics; Fx hands out tagged tokens (word/path/url/wide/tabs/prose) so every field is checked separately.
- Tool cards (cards.rs): header summary cut with … by right_align (e.g. '/private/var/folders/n6/hn37q6p…'); body lines cut by truncate_line; spill pointer cut; card wider than its area below 12 cols (width.max(12)); copy buttons silently dropped when the bottom border was too short; compact cards (todo/memory/spawn_agent) cut their summary and expanded items; generic arg summaries capped at 30/40 chars, memory/spawn_agent at 60, task titles at 50; parallel-group header cut. Fix: card_top keeps icon+name in the top border and wraps the rest (summary, badges, and the right badges when they do not fit) inside the box; card_row wraps body lines inside the box with a blank line-number gutter on continuation rows (soft-wrap marker kept first, before the border); overflow buttons get rows above the bottom border (same hit/keyboard ids); wrap_right for compact headers and the group header; caps removed.
- Subagent card (transcript.rs): header (name, agent, model, host) cut; body lines cut; min width 12 overflowed narrow panes; '… N more lines' without a hint. Fix: card_top/card_row/card_bottom; hint '… N more lines · Enter to open the agent'; hit covers all rows.
- Notices (verify failed etc.): header and detail lines cut. Fix: wrap_right header, wrapped indented details.
- Assistant meta line (model · tokens · cost · stop): cut. Fix: wraps.
- User message header: first row wrapped at max(10) cells and could overflow narrow widths. Fix: wrap_line_with (first row leaves room for copy+time, the rest uses the full width).
- Markdown code blocks: code lines cut with …; long language label cut. Fix: code lines wrap under the │ bar; a long label wraps and the ⧉ copy button gets its own row (CodeHeader updated). Tabs in code now go through sanitize (4-col stops).
- Activity line (indicators.rs + chat view): one row, cut mid-word ('ctrl+c to in'); running todo calls showed raw JSON args. Fix: wraps onto more rows (height only grows during a turn so the transcript does not jump; at most a quarter of the screen); running compact tools use compact_summary. Terminal title keeps its 60-char cap (not on screen).
- Empty-transcript hint ('Type a message and press Enter. Ctrl+P o'): cut. Fix: wraps above the input.
- Status line (status.rs + chat view): one row; cwd cut to 40 cells; low-priority segments silently dropped. Fix: StatusLine::layout packs segments onto up to 3 rows (max a sixth of the screen), wraps long values, and when segments are still left out ends with a '+N more' button (status.more, Link token; click or Shift+Tab+Enter) that opens a 'Status' panel with everything (also the palette entry 'Show status', AppCommand::ShowStatus). The MCP indicator keeps its short label (the full names are in /mcp and in the Status panel).
- Footer and keyboard selection bar (app.rs): one row; at narrow widths the 'keys' button was drawn over the run state and the selection hints were cut. Fix: footer items wrap onto more rows (keys right-aligned on the last row; buttons keep their hit regions); selection hints wrap at ' · '.
- Text helpers (chat/text.rs): wrap_lines, wrap_text, wrap_line_with, wrap_right, take_wrap_marker; sanitize expands tabs to 4-column stops.

Inventory, part 2 (pickers):
- Command palette rows: titles/descriptions cut with … (past-run titles cut in the middle), long scopes/keys ran past the row. Fix: rows stay one line so columns align; when the selected row is cut the bottom border shows '→ full text'; Right or a click opens the full title, description and source/key wrapped below the rows (new item_detail.rs widget), Shift+↑↓ or ▲/▼ scrolls it with 'shift+↑↓ a-b/n', Right or '← hide full text' closes it.
- Palette query line and argument prompt: one row, cut. Fix: wrap by character with the cursor kept after the typed text.
- Slash completion: name/description cut; scrolled by MAX_COMPLETIONS not the real height (selection could hide). Fix: the selected command shows in full (all its rows clickable); window from the real height (Input::completion_height_at).
- Input box: Up/Down moved by logical line (middle rows of a long wrapped line unreachable except char by char); raw control chars. Fix: Up/Down move by display row keeping the column (history recall on first/last row unchanged); control chars become spaces.
- /model picker: cells, provider error headers, hints, 'no models' cut. Fix: hints/notice/messages/query wrap; '→ full text' detail with every cell labelled plus the refresh error.
- /resume picker: name, details, description, hints cut. Fix: hints/query wrap; full-text detail; dialog grows by the detail height.
- fuzzy.rs: no findings.

Inventory, part 3 (overlays):
- Panel (/cost, /config, /mcp, /jobs, /help, Status): long lines clipped. Fix: wraps; scroll and ' a-b/n ' count wrapped rows; long titles show above the text; wide button labels wrap.
- Toasts: title/body cut to one row each. Fix: wrap up to 6 rows, then '… more in Notifications (palette)'; shrink to the room left instead of vanishing.
- Notification history: rows cut, fixed height. Fix: items wrap (time on the first row), popup sizes to content, PgUp/PgDn/wheel scroll, Up/Down/Home/End/click select and scroll into view, position in the border.
- Approval cards: subject/reason cut (reason at 30 chars), buttons ran past the edge, preview stopped at 12 lines, tall cards hid their top. Fix: subject/reason/rules wrap inside the card, buttons flow onto rows, '… N more lines' row is a button (click or e), '↕ a-b/n · pgup/pgdn or wheel to scroll' with PgUp/PgDn/Home/End/wheel.
- IniConfirm: one row cut. Fix: wraps, height(width).
- Login modal: options/field/title cut; body cut at small heights. Fix: wrap (cursor at the end of the field's last row); selection kept in view, PgUp/PgDn/wheel, ' a-b/n '.
- Help overlay: rows cut. Fix: keys and descriptions wrap in their columns; Up/Down/PgUp/PgDn/Home/End/wheel scroll with '↕ a-b/n'.
- Chat selection bar (chat/copy.rs): buttons fell off screen when narrow. Fix: buttons right after SELECT, bar wraps (bar_height(width)).
- Memory card / memory browser: note and bullets cut, min width 12, tabs/buttons dropped, drive path cut, rows and detail cut, detail could not scroll. Fix: wraps everywhere, min width 4, detail scrolls (PgUp/PgDn/Home/End/wheel) with position; at 40x12 the body becomes one scrolling column.
- harness/src/tui (mcp.rs, jobs.rs, mcp_login.rs, commands.rs): no space-driven cuts; data caps (untrusted MCP labels 120 chars, reasons 160 with the full text behind [logs], jobs output tail pointing to the log, 20 recent runs in the palette with all in history, hygiene toast 4 lines with the full report in node detail) are not layout; their text now wraps through the panel.

Inventory, part 4 (screens):
- Map header: run name/metrics cut. Fix: wraps (up to half the height).
- Map node boxes: name/agent/model/host cut or dropped. Fix: when the selected node's box cuts anything an info line under the graph shows everything plus '(Enter: detail · Space: live)'; if even that has no room: '… more: Enter opens the detail'.
- Live pane: title ran under [x]; notes/tool summaries/output cut; only the last 4 output lines with no hint. Fix: wraps; '… N earlier lines (Enter: node detail)'.
- Node detail: header, turn/verify/notice headers, verify output (last 20 lines only), meta/hygiene/artifacts, sidebar cut; min width 10 overflowed. Fix: everything wraps, header rows beyond height-3 scroll with the log, sidebar scrolls keeping focus in view, Home/End added.
- Goal bar pane: goal limited to 3 rows, titles/owners cut, tasks below the pane unreachable. Fix: wraps; pane scrolls (wheel, PgUp/PgDn/Home/End) with ' a-b/n '; Up/Down reach every task.
- Goal bar collapsed indicator: title cut to 24. Fix: full title on the status line (which wraps); the indicator is now a status button (status.goal) that opens the Status panel listing the goal and every task.
- Diff review: header/file rows/sources cut, files beyond the pane not drawn, diff lines cut. Fix: wraps (diff lines under a blank gutter, marker first, wrap_diff_line), file list scrolls (wheel, Up/Down, focus kept in view) with the longest position hint that fits ('files a-b/n · ↑↓ or wheel').
- Inspector: tab/[export] buttons dropped, JSON/streamed text clipped, layer labels cut. Fix: title/buttons flow onto rows, body and labels wrap.
- Context view: rows cut, expanded text limited to 12 lines, no scroll to focus. Fix: wraps, Up/Down reach every row and scroll to it.
- History: run column cut, rows clipped, off-screen runs unreachable by keyboard. Fix: label wraps in its column (stacked layout when narrow), scrolls by rows, Up/Down through all runs.
- Replay bar: buttons dropped when narrow. Fix: flows onto rows (ReplayBar::height).
- ui-demo gallery tabs: tabs that did not fit were dropped. Fix: the tab row wraps.
- Left as is (naming, not layout): RunSummary::outcome() 60-char error summary, names::shorten run labels; history/show.rs is CLI output.

Tests and validation:
- Layout matrix (crates/harness-tui/src/layout_matrix.rs + chat/overlays/pickers/screens submodules, plus context_view_fits_and_everything_is_reachable and replay_bar_fits_at_every_size in their modules): every view at 40x12, 60x20, 80x24, 120x40, 250x60 asserting no border problem, no raw control char and every tagged fixture token visible or reachable via the documented action; 1x1, 8x3, 20x5, 30x8 for panics. Slowest case ~2.4s; whole harness-tui suite ~4-6s.
- New mouse+keyboard tests for every new affordance (status +N more / Show status / goal indicator, footer wrap + keys button, palette/model/resume full text, completion, toasts, notification history, approvals scroll+preview, help, login, panel, selection bar, memory detail, goal bar scroll, diff file list, map info line, detail headers, context view, history, replay bar, gallery tabs).
- Existing tests adapted only where they asserted the old cut layout or the old row positions: cards header_has_icon_name_summary_duration_timestamp_and_tint (rule row moved down, now also asserts the wrapped summary), selection_tests copying_a_truncated_tool_card_header_leaves_out_its_rule (header no longer cut; still asserts no chrome copied), selection_tests dragging_past_the_top_of_the_chat_scrolls_and_extends_the_selection (top row may be copied from the pointer column; still asserts every row in order), chat view resizing_a_scrolled_up_view_keeps_the_reading_position (precondition uses the first text row), app goal_bar_shows_in_both_views (status may take two rows), map a_long_title_and_the_header_metrics_wrap_at_80_columns (was ..._is_cut_...), model_picker check_layout (counts wrapped hint rows). ui-demo --print now renders at 120x44 (was 120x40) so the whole scripted chat still fits with the wrapped status line.
- Snapshots changed after review (intended wrapping only): chat_short, chat_long, chat_resized, chat_scrolled_up (status line shows path+branch on a second row instead of dropping them), picker_80x24, picker_120x30 ('→ full text' hint), detail_gallery, gallery_detail, gallery_map, gallery_goal_bar, gallery_history.
- cargo fmt --all --check: clean. cargo clippy -p harness-tui -p harness --all-targets --locked -- -D warnings: clean. cargo nextest run -p harness-tui -p harness --locked: 645/645 passed. Under heavy parallel load, timing tests (model picker <50ms open, palette/fuzzy 1000-entry frame budgets, tui::tests::model_without_argument_and_the_palette_entry_open_the_picker <50ms) failed once each and pass on rerun; unrelated to layout.
- tmux (private server -L t160, temp HOME/XDG/HARNESS_CONFIG, --fake-script with long words/URL/CJK/tabs and a bash tool call needing approval): at 50x15 the empty hint wraps, status wraps to 2 rows with '+1 more', palette 'Show status' opens the Status panel (full path wrapped) and scrolls, approval card wraps with the scroll hint, bash/code cards wrap inside their borders, map node box + node detail fit, /help panel wraps and scrolls (1-11/64 … 54-64/64). At 160x45 everything fits on one row per item, card headers keep a gap before the rule.
- Known limits: the activity line is capped at a quarter of the screen (its details are in the card and status line); on a tiny screen a panel can cut its top button rows (close stays, Esc closes); the history run column is fixed at 44 cells.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Audited every TUI view for wrapping and overflow (inventory in the notes) and fixed every finding: text now wraps instead of being cut (cards, subagent cards, notices, markdown code, activity line, panels, toasts, notification history, approvals, login, help, memory browser, map header/live pane, node detail, goal bar, diff, inspector, context, history, replay bar, gallery tabs); fixed one-row areas grow (status line up to 3 rows, footer, selection bars) and keep the rest one action away with a hint (status '+N more' button and palette 'Show status' panel, clickable goal indicator, palette/model/resume '→ full text', map info line, '… N more lines' expands); popups size to content and scroll with position indicators by wheel and keys. Added the test-only layout matrix (crates/harness-tui/src/layout_matrix.rs + submodules) rendering every view with tagged long-content fixtures at 40x12/60x20/80x24/120x40/250x60 (plus tiny sizes) asserting no content on/past a border, every token visible or reachable via the documented action, no control chars and no panics, plus mouse+keyboard tests for each new affordance. Docs: docs/tui.md layout rules section, tui-views.md status line and cards, CHANGELOG Unreleased. Verified with cargo fmt --check, clippy -D warnings (harness-tui, harness) and nextest (645/645), and in a private tmux server at 50x15 and 160x45.
<!-- SECTION:FINAL_SUMMARY:END -->
