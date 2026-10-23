---
id: TASK-145
title: 'Refine session resume: readable names with last-activity time and description'
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 04:24'
updated_date: '2026-10-23 05:47'
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
- [x] #1 Each session has a short name (a few words, derived from the conversation, editable via a /rename command) and a one-line description summarising what it was about, updated as the session progresses
- [x] #2 Session lists (/resume picker, harness sessions, palette) show name, the date and time of the last message (relative for recent ones, e.g. '14:32 today', 'yesterday 09:10', 'Oct 21 16:05'), project, model, message count and cost, newest activity first
- [x] #3 The /resume picker is a fuzzy-searchable dialog over names and descriptions, fully clickable and keyboard-operable; harness run --resume accepts last, a name, or an id prefix
- [x] #4 Generating names and descriptions uses no extra model call by default (derived locally); an optional cheap-model summary can be enabled in harness.ini
- [x] #5 Tests cover name and description derivation, last-message timestamps and their formatting (including timezone and day boundaries), ordering, rename, fuzzy search and resume by name, plus a pseudo-terminal test of the picker
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
1. harness-events::names: session_name (short name from a prompt, a few words) and activity_label ('14:32 today', 'yesterday 09:10', 'Oct 21 16:05', year when not this year) with timezone/day-boundary tests.
2. harness-agent::session: SessionMeta gains name, renamed, description, last_message_at, messages (serde defaults for old files); record() derives name/description locally from the conversation; list() orders by last activity; find() accepts last, id, id prefix, #tag, name (exact, then unique prefix); rename(); one-line listing with name, time, project, model, message count, cost, tag, description; optional cheap-model summary request/reply helpers.
3. harness-config: [general] session_summary_model (off by default).
4. harness core: rename(), optional summary call after a turn; /rename builtin; /resume without an argument opens a picker; palette gets recent sessions; harness sessions uses the new listing; run --resume via find().
5. harness-tui: SessionPicker modal (fuzzy over name/description, clickable rows, wheel, keyboard), CoreEvent::SessionPicker, UiRequest::Resume.
6. Tests: unit (names, derivation, ordering, rename, find), app tests for picker keyboard + mouse, headless resume by name, pty test of the picker. Docs: sessions.md, commands.md, cli.md, harness-ini reference, doc-5, CHANGELOG.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented:
- harness_events::names: session_name (<=5 words, openers/articles dropped, UNNAMED_SESSION fallback) and activity_label ('14:32 today', 'yesterday 09:10', 'Oct 21 16:05', 'Oct 21 2025 16:05'; calendar days in the given offset). Reused title_from_text, shorten, short_tag/matches_tag from the names lane.
- harness_agent::session: SessionMeta gains name, renamed, description, last_message_at, messages (serde defaults; old session.json files load). refresh_from() runs in record(): derives title/name from the first prompt (never after /rename) and the description from first + latest request + edited files (files kept visible when cut). list() orders by last_message_at (fallback updated_at). find(): last (skips sessions without a prompt), id, name (ci), id prefix, #tag, unique name prefix; Ambiguous on duplicates. rename(), clean_name(), is_empty(). Optional summary helpers summary_transcript/summary_request/parse_summary; SessionHandle::apply_summary keeps user-chosen names.
- harness-config: [general] session_summary_model (default off); reference.ini, doc-2, guide updated; CHANGELOG Defaults entry with fingerprint 00feb6d0d4e5.
- harness-tui: new session_picker module (SessionPicker, SessionRow, two rows per session, fuzzy over name/description/project/tag, keyboard + click + wheel); CoreEvent::SessionPicker, UiRequest::Resume; EntryKind::Session and ids::SESSION_PREFIX in the palette (additive).
- harness: new sessions.rs (picker rows, palette entries, local offset read in main before threads exist, summary model call with redaction and 30s timeout); /rename builtin; /resume without args opens the picker, with args resolves via find and keeps the current session on errors; palette lists 10 recent sessions; harness sessions prints the new one-line listing and hides prompt-less sessions; --resume help text updated.
- Tests: names unit tests (tz/day/year boundaries), session unit tests (derivation, ordering, rename, find, legacy files, summary parsing), picker unit + app tests (keyboard, mouse, wheel, paste, outside click), driver test (rename, picker rows, palette, resume by picker/palette/name), headless tests (listing, resume by name and tag, summary model via fake provider), PTY test tests/resume_pty.rs (filter, Enter, Esc, click).
- Updated existing expectations only where the builtin list grew: commands tests now include 'rename' (Builtin::ALL and the 're' completions).
Validation: cargo nextest run --workspace --no-fail-fast: 1533 passed, 8 skipped; cargo test --workspace --doc ok; clippy -D warnings clean for touched crates; cargo fmt --check clean; rustdoc -D warnings clean. PTY test run 3x, passing each time.

Review fixes (both blocking findings verified as real and fixed):
1. Verifier feedback / compaction handoff counted as requests. Confirmed: user_prompts()/message_count() took every user text. Fix: session::is_harness_text() classifies user-role text the harness wrote (provenance-marked blocks via harness_policy::provenance::TAGS, compaction handoff via new compaction::HANDOFF_OPEN, unmarked verifier feedback via new agent_loop::VERIFY_FEEDBACK_PREFIX); user_prompts, message_count and summary_transcript look at text blocks one by one, so a prompt merged into the handoff message still counts. Content-based rather than recorded on SessionMeta so resumed (rebuilt) conversations and old sessions classify the same way. Tests: session::tests::verifier_feedback_and_handoffs_are_not_requests (labelled + unlabelled feedback, compacted conversation, user text with angle brackets), headless verifier_feedback_is_not_a_request_in_session_lists (the reviewer's repro: exit 7, max_fix_attempts = 1 -> 3 msgs, description 'fix the header parser').
2. Inline, uncancellable, unrecorded summary call. Confirmed. Fix: Core::turn now spawns the summary in the background (skipped after a cancelled turn and while a budget is exhausted); the task saves to the session file via SessionStore::update and Core::settle_summary mirrors it into the live handle. A new turn, /rename (Core::rename is now async) and Core::finish cancel it; headless waits for it via Core::wait_summary unless SIGINT. sessions::summarize takes a cancel token (select + child token, 30 s timeout kept) and records a session-summary child node of the turn: node_started, model_call_started, model_call_finished with usage, cost, node_finished. Cost is charged to the shared BudgetMeter (hooks() now returns it) and added to the session's tokens/cost through SummaryUpdate / SessionMeta::apply_summary. Child node means rebuild_conversation ignores it (headless test resumes after a summary). Tests: sessions::tests summaries_come_from_the_model_and_are_redacted (events + cost), a_cancel_stops_a_summary_in_flight; tui::tests the_summary_runs_after_the_turn_and_is_recorded_with_its_usage, a_slow_summary_never_delays_turns_or_closing_and_cancelled_turns_skip_it (60 s fake delay, 20 s bounds); headless summary test extended. docs/sessions.md and CHANGELOG updated.
Validation: cargo nextest run --workspace: 1538 passed, 8 skipped; clippy -D warnings clean for harness-agent and harness; fmt clean; rustdoc -D warnings clean; new tests run 3x green.

Integration (2026-10-23): workspace green locally after all three lanes merged in the tree: cargo fmt --all --check clean; cargo clippy --workspace --all-targets --locked -D warnings clean; cargo nextest run --workspace --locked --profile ci 1538 passed, 8 skipped (pre-existing gated tests); cargo test --doc ok; cargo deny check ok; tests/install-test.sh ok; shellcheck on install.sh, scripts/*.sh, tests/*.sh clean. The earlier history.rs/palette.rs failures are green. No tests removed (55 #[test] added, 0 removed). DoD #1 left unchecked until CI passes on the pushed commit.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Sessions now have a short name (from the first prompt, editable with /rename), a one-line description that follows the conversation, the last-message time and a message count, all derived locally; [general] session_summary_model optionally lets a cheap model write them. /resume without an argument opens a fuzzy-searchable, clickable picker; the palette and harness sessions show name, last activity (14:32 today / yesterday 09:10 / Oct 21 16:05), project, model, messages, cost and description, newest activity first; --resume and /resume accept last, a name, an id prefix or a #tag. Verified with unit tests (names, session derivation, ordering, rename, find), app and driver tests for the picker, headless binary tests, a PTY test of the picker, and a green workspace run (1533 passed).
<!-- SECTION:FINAL_SUMMARY:END -->
