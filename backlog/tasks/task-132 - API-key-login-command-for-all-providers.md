---
id: TASK-132
title: API key login command for all providers
status: Done
assignee: []
created_date: '2026-10-22 19:47'
updated_date: '2026-10-22 21:56'
labels:
  - provider
  - config
  - security
  - problem-8
milestone: m-1
dependencies:
  - TASK-2
  - TASK-3
  - TASK-9
  - TASK-113
references:
  - 'https://code.claude.com/docs/en/legal-and-compliance'
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 132000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
API keys work today, but only by hand-editing harness.ini and remembering to chmod it. Users need a guided way to add, test and remove keys for Anthropic, OpenAI and OpenRouter. Claude subscription login is deliberately out of scope: Anthropic's terms forbid third-party apps from offering Claude.ai login or routing requests through Free, Pro or Max credentials, so Anthropic is API key only.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 harness login <anthropic|openai|openrouter> and a /login palette action prompt for an API key with masked input that is never echoed, logged or stored in events
- [x] #2 The key is validated with a cheap authenticated request (for example listing models) before it is saved, with a clear error on failure
- [x] #3 The key is written to the provider section of harness.ini without disturbing other lines or comments, and the file is set to mode 0600
- [x] #4 Users can choose api_key, api_key_cmd or api_key_env instead of an inline key
- [x] #5 harness logout <provider> removes the stored key after confirmation
- [x] #6 doctor shows the auth method in use per provider
- [x] #7 Docs list supported auth per provider: Anthropic API key only (subscription login not supported, with the reason), OpenAI API key or Sign in with ChatGPT, OpenRouter API key
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
1. harness-config ini::edit: line-preserving set/remove of api_key/api_key_cmd/api_key_env in [provider.<id>], atomic private (0600) write that follows symlinks.
2. harness-config login: auth-support table, Method, MaskedInput (zeroizing, never printed) and a keyboard/click-driven LoginFlow state machine the TUI /login action can drive.
3. harness-providers login: verify_api_key with a cheap authenticated GET (anthropic v1/models?limit=1, openai models, openrouter key) and clear errors.
4. crates/harness login.rs: shared apply() (resolve -> verify -> save) plus harness login/logout CLI with masked tty input, piped stdin, --api-key-cmd/--api-key-env, logout confirmation.
5. ProviderConfig auth origin + doctor shows auth method per provider.
6. Docs: docs/providers.md auth table, cli.md, doctor.md, doc-2 reference; tests for each AC.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented (providers lane):
- harness_config::ini::edit: set_credential/remove_credentials/credential_keys (line-preserving, CRLF-aware, removes competing credential keys in the section only), save_credential/remove_credential_file, write_private (atomic temp+rename, 0600, follows symlinks, 0700 parent dirs). Values that would not round-trip (line breaks, ' ;', control chars, bad env names) are refused.
- harness_config::login: AUTH_SUPPORT/auth_support, Method, MaskedInput (zeroizing, Debug shows length only, mask with •), LoginFlow state machine (Provider -> Method -> Value -> Verifying -> Done/Failed) driven by LoginKey (Up/Down/Enter/Esc/Char/Backspace/Paste) and click(index); returns LoginEvent::Submit(LoginRequest); finish(Result) reports back.
- harness_providers::login: verify_api_key / KeyVerifier (anthropic GET v1/models?limit=1, openai GET models, openrouter GET key since its model list is public), errors scrubbed of the key; OpenRouter key label deliberately not surfaced (it is a partial key).
- crates/harness/src/login.rs: apply() (resolve cmd/env -> verify -> save) shared by CLI and TUI; harness login <provider> [--api-key-cmd|--api-key-env], terminal method menu, raw-mode masked entry, piped stdin; harness logout <provider> [--yes] with [y/N] confirmation. Wired in main.rs (additive).
- ProviderConfig::resolve_api_key_with_origin + auth_description; SecretOrigin::ini_key; doctor prints 'auth: API key (api_key_env VAR)' / '(api_key_cmd)' / '(api_key in harness.ini)' / 'auth: none' per provider with a harness login fix.
- Docs: docs/providers.md#logging-in (supported auth table incl. Anthropic subscription-login reason), docs/cli.md, docs/doctor.md, docs/guide/harness-ini.md, CHANGELOG, doc-2 via backlog doc update.
Verification: cargo test -p harness-config (124 lib + doctest), -p harness-providers --test login_verify (4 + 1 ignored live), -p harness-eval doctor (5), -p harness --bin harness login (7), -p harness --test login (4, incl. pty test proving the typed key is never echoed and only • is shown). Live: login_verify live test with OPENROUTER_API_KEY passed (valid accepted, bogus rejected as Auth), and harness login openrouter --api-key-env OPENROUTER_API_KEY against a temp HARNESS_CONFIG succeeded; no Anthropic/OpenAI live check was possible (no keys).
Clippy -D warnings clean for harness-config, harness-providers, harness-eval, harness.
Open: AC1's /login palette action is not wired: the built-in command enum lives in harness-agent (remote lane) and its dispatch in crates/harness/src/tui (tui lane). The TUI needs: a Builtin::Login (or palette entry) that opens a modal rendering LoginFlow (title/options/selected/field/message/note/hint), maps keys to LoginKey and option clicks to click(i), on Submit spawns crate::login::apply(&crate::login::config_path(), &req, &LiveVerifier::default(), &env) and calls finish(); and should add the new key to the redactor. DoD4 (UI interaction tests) depends on that. Unrelated failures seen: tui::tests::model_without_argument_and_the_palette_entry_open_the_picker (tui lane in progress), harness-eval process_executor test flaked once and passed on rerun.

Review fixes (round 2):
- /login wired into the TUI: Builtin::Login ('/login [anthropic|openai|openrouter]', also palette entry cmd.login) in harness-agent; harness_tui::login::LoginModal renders LoginFlow with theme tokens, registers each option (login.option.N) and button (login.primary / login.back) with the interaction layer; keys map to LoginKey (Up/Down/Tab/BackTab, Enter, Esc, Backspace, chars), paste goes to the field, a click outside backs out. App: CoreEvent::Login{provider} opens it, UiRequest::Login(LoginSubmission) carries the request (Arc, Debug shows provider + ini key only), CoreEvent::LoginResult feeds finish() or a toast if the modal was closed. Driver (crates/harness/src/tui/commands.rs::login) runs login::apply in the driver task (not the UI task), registers an inline key with the redactor before verifying, and on success reloads that provider's section, adds the resolved key to the redactor and drops the cached model catalog.
- apply/login_main env resolver is now '&(dyn Fn(&str) -> Option<String> + Sync)' so the driver future stays Send.
- Flaky test fixed: ini::edit::tests::saving_and_removing_never_log_the_key. Root cause verified in tracing-core 0.1.36 (callsite.rs Dispatchers::rebuilder): with only one live dispatcher (has_just_one), a callsite first hit on another test thread is judged by that thread's empty default and cached as never, so the scoped subscriber missed 'credential saved'. Fix keeps both assertions: registers the callsites first, keeps a second (NoSubscriber) dispatcher alive, installs the subscriber with set_default and calls rebuild_interest_cache. Before: 10/12 lib runs failed even with rebuild alone; after: 0/20.
- Tests: harness-tui login::tests (7: keyboard, Esc, mouse, paste/unknown provider, theme tokens, app keyboard, app mouse + toast), harness tui::tests::login_opens_the_modal_and_saves_a_checked_key_without_leaking_it (palette entry, /login and /login openrouter, rejected key saves nothing, accepted key saved 0600 and resolved, both keys redacted, a new session stores a prompt quoting them masked, no UI event / run event / data-dir file contains either key).
- Verification: cargo test --workspace (0 failures), cargo clippy --workspace --all-targets -D warnings clean, cargo fmt --all --check clean, cargo test -p harness-config --lib x20 green. Live: none added (no Anthropic/OpenAI keys; TUI verify uses a fake Verifier).
- Docs: docs/providers.md 'In the TUI: /login', docs/commands.md row, CHANGELOG. Note: cargo fmt --all ran across the workspace.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
API key login for all providers: harness login/logout CLI (masked entry, cmd/env options, cheap verification, line-preserving 0600 save) and a /login TUI modal (keyboard and mouse) over the shared LoginFlow and login::apply; the key is redacted and never stored in events; doctor shows the auth method; docs list supported auth per provider. Verified with workspace tests (incl. new harness-tui login tests and the TUI driver login test), clippy and fmt; live check only against OpenRouter.
<!-- SECTION:FINAL_SUMMARY:END -->
