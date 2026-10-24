---
id: TASK-163
title: '/effort command: set reasoning effort for the session, across providers'
status: In Progress
assignee:
  - '@claude'
created_date: '2026-10-23 23:58'
updated_date: '2026-10-24 00:30'
labels:
  - feature
  - tui
  - providers
dependencies: []
priority: high
ordinal: 177000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Reasoning effort can only be changed today by copying an agent's YAML into the global agents folder and adding effort: low|medium|high. There is no quick way to try a lower or higher effort in a running session. The TASK-161.7 live A/B showed effort is a real speed/cost lever (coder at low: turn p50 -24%, eval cost -40%), so users need to switch it in the TUI. Each provider maps effort differently: Anthropic sends adaptive thinking plus output_config.effort (Haiku 4.5 converts it to a thinking budget), and OpenRouter and OpenAI send reasoning.effort. Models that do not reason must be handled. See docs/providers-*.md and docs/performance.md.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 /effort with no argument shows the active effort for the current agent and model (including 'model default' when none is set) and opens a picker listing default/low/medium/high
- [x] #2 /effort low|medium|high|default sets the effort for the rest of the session on the active agent; the next model request carries it, and 'default' restores the agent YAML or model default
- [x] #3 The override reaches the wire correctly on Anthropic (adaptive and budget models), OpenRouter and OpenAI, proven by request snapshots or unit tests per provider
- [x] #4 On a model that does not support reasoning, /effort says so clearly and does not send reasoning fields that would make the request fail
- [x] #5 The active effort is visible in the TUI (status line or model area) and updates immediately; it is recorded in the session so resume restores it
- [x] #6 The command appears in the command palette and /help with completion for its arguments
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
Scope: /effort changes the session's main agent only (subagents keep their own YAML effort via harness_agent::subagents::thinking_for). Defaults stay unchanged: with no override every request is byte-identical (golden snapshots + defaults_changelog fingerprint untouched).

1. Command definition (harness-agent/src/commands/builtin.rs): add Builtin::Effort after Model ("effort", "Show or set reasoning effort for this session's agent"), arg ArgSpec::optional("effort").with_choices(["default","low","medium","high"]). Update ALL array + the every_builtin test list. /help and the palette pick it up from the registry automatically.
2. Session persistence (harness-agent/src/session.rs): SessionMeta gets `#[serde(default, skip_serializing_if = Option::is_none)] effort: Option<Effort>` (None = no override, so old session files load and new ones without an override are unchanged); SessionHandle::set_effort(Option<Effort>) saves. Unit test: round trip + old file without the key.
3. Core (harness/src/core.rs, wiring.rs): wiring::thinking(agent) becomes thinking(agent, override) = override.or(agent.effort) -> ThinkingConfig. Core::start reads override from the resumed meta; new Core::set_effort(Option<Effort>) updates config.thinking (next request + prewarm use it) and the session meta; Core::effort_state() -> {override, agent default, effective, model_reasons: provider.capabilities(model.id).thinking}. Re-evaluated after /model.
4. Provider gating for non-reasoning models: Anthropic (ThinkingStyle::None) and OpenAI (is_reasoning_model) already drop reasoning fields; OpenRouter sends `reasoning` unconditionally today -> drop request.thinking in OpenRouterProvider::request_body/stream when the catalog knows the model and supported_parameters lacks `reasoning` (unknown models unchanged). Unit test in openrouter.rs.
5. Wire proof (AC3): new crates/harness/tests/effort_wire.rs (or wiring unit tests): an agent with/without YAML effort + override -> ThinkingConfig -> request_body() of AnthropicProvider (claude-opus-5.5 adaptive: output_config.effort; claude-haiku-4-5 budget: thinking.budget_tokens = effort_budget), OpenRouterProvider (reasoning.effort; non-reasoning catalog model: no reasoning), OpenAiProvider (gpt-5: reasoning.effort; gpt-4o: none). 'default' restores YAML effort / no fields.
6. TUI driver (harness/src/tui/commands.rs, tui.rs): Builtin::Effort handler. With an arg: parse, call core.set_effort, toast "effort low" (or "model default"); on a non-reasoning model a Warning toast says the model does not reason so nothing is sent (override kept for a later /model switch). While a turn runs, queue the command like /model. Without an arg: open a panel "Reasoning effort" listing agent, model, active effort with its source (session override / agent YAML / model default) and how this provider maps it (or that the model does not reason), with buttons [ default ] [ low ] [ medium ] [ high ] (ids effort.set:<v>); panel_action applies and refreshes the panel text (PanelText) + toast. Panel buttons are already clickable and keyboard-operable (Tab/Left/Right, Enter, Esc).
7. Status line (harness-tui app.rs/status.rs): new CoreEvent::Effort(Option<String>) -> StatusLine.effort rendered in the model segment as `model · effort low · mode` (nothing when no effort is set, so existing snapshots stay; `no reasoning` when an effort is set on a model that does not reason). Driver sends it at start/resume, after /effort and after /model.
8. Argument completion (harness-tui input.rs): SlashCommand gains `choices: Vec<String>` (first-arg choices, filled in slash_commands() from the registry spec); typing `/effort ` + partial opens inline completion of the choices (Up/Down, Tab/Enter accept, Esc closes, rows clickable; Enter on an exact choice sends). Generic, so /mode, /cost etc. benefit. Update the ~13 SlashCommand literals.
9. Tests: builtin registry/help/palette entry for /effort; core: /effort low -> next ModelCallStarted payload thinking.effort=low, /effort default -> agent YAML (researcher low) or none; resume restores override (temp data dir, fake provider); non-reasoning fake (capabilities thinking:false) -> warning toast; picker panel driven via the real App on TestBackend: Enter/Tab-to-button and mouse click both send UiRequest::PanelAction(effort.set:..) and change effort; status line unit + styled snapshot; input arg completion keyboard + click tests.
10. Docs: docs/commands.md (/effort), docs/guide/palette.md if it lists commands, docs/tui-views.md (status segment), providers-anthropic/openrouter/openai.md (session override + non-reasoning handling), docs/performance.md (point to /effort instead of copying YAML), docs/data-dir.md if session meta fields are listed. No new ini keys. CHANGELOG entry under the feature section (not Defaults).
11. Verify: cargo fmt, clippy -D warnings and tests for harness-agent, harness-providers, harness-tui, harness; cargo test -p harness-eval --test golden_requests and --test defaults_changelog (must be unchanged); workspace tests at the end. Optional: one targeted live OpenRouter Opus 5.5 request at /effort low to confirm the wire (< $0.05).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented in worktree /home/admin2/CODE/harness-effort, branch task-163-effort (base task-163-base 206dbc4, not pushed):
- 9c52e4e OpenRouter: leave reasoning out for catalog models that do not reason
- 9dd5d8a Slash completion offers a command's argument choices (generic: /effort, /mode, /cost, /config, ...)
- bcafb69 TUI: run slash commands queued during a turn instead of sending them as prompts (pre-existing bug: a queued `/model NAME` was sent to the model as a prompt; needed for /effort while busy)
- aa5f975 /effort feature: Builtin::Effort, SessionMeta.effort (skip if None) + SessionHandle::set_effort, wiring::thinking(agent, session), Core::effort()/set_effort() + EffortState, Model.thinking (registry style), tui/effort.rs (panel with default/low/medium/high buttons, PanelText refresh, toasts, warning on non-reasoning models), CoreEvent::Effort -> StatusLine.effort (`model · effort low · mode`, `no reasoning`), status sent at start/resume and after /model
- docs commit: docs/commands.md (#effort), tui-views.md, sessions.md, providers-anthropic/openai/openrouter.md, performance.md, CHANGELOG (Added + Fixed; not Defaults)

Decisions: main agent only (subagents keep their YAML effort); /new and agent switch reset; override kept on a non-reasoning model and applies after /model; reasons = provider capabilities().thinking || registry style != None (an OpenRouter model missing from both the built-in table and the catalog would be reported as not reasoning, while the provider still sends reasoning for unknown models).

AC evidence (tests):
- AC1: tui::tests::effort_picker_works_with_keyboard_and_mouse (panel shows `effort  model default`, buttons), effort_on_a_model_without_reasoning_warns
- AC2: tui::tests::effort_sets_the_next_request_shows_in_the_status_and_survives_resume (recorded ModelCallStarted thinking null -> {effort: low} -> null after default), effort_default_restores_the_agent_yaml_effort (researcher low restored)
- AC3: wiring::effort_wire_tests::* (real provider_sync bodies: Opus 5.5 output_config.effort, Haiku 4.5 budget_tokens, OpenRouter + OpenAI reasoning.effort), harness-providers openrouter::tests::reasoning_is_left_out_for_catalog_models_that_do_not_reason
- AC4: effort_on_a_model_without_reasoning_warns, effort_wire_tests::models_that_do_not_reason_get_no_reasoning_fields, OpenRouter gating test
- AC5: status::tests::footer_shows_the_reasoning_effort_after_the_model, resume part of the AC2 test, session::tests::effort_override_is_saved_and_old_sessions_have_none
- AC6: tui::tests::effort_is_in_help_palette_and_completion (help, palette run with arg, slash choices, busy queue), input::tests::argument_choices_complete_with_the_keyboard, argument_choices_are_clickable
- DoD4: picker keyboard (Right+Enter, Esc) and mouse click tests; completion keyboard + click tests.

Gates in the worktree: cargo fmt --check OK; cargo clippy --workspace --all-targets -D warnings OK; cargo nextest run --workspace 2059 passed, 10 skipped; cargo test --workspace --doc OK; harness-eval defaults_changelog and golden_requests pass unchanged (no default or snapshot change). No live provider calls were made (the OpenRouter reasoning.effort wire was already exercised live in TASK-161.7). DoD1 left unchecked pending CI after merge.
<!-- SECTION:NOTES:END -->
