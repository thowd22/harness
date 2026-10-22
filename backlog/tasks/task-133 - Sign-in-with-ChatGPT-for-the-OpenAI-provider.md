---
id: TASK-133
title: Sign in with ChatGPT for the OpenAI provider
status: Done
assignee:
  - '@claude'
created_date: '2026-10-22 19:47'
updated_date: '2026-10-22 22:58'
labels:
  - provider
  - security
  - problem-8
milestone: m-1
dependencies:
  - TASK-12
  - TASK-3
  - TASK-25
  - TASK-132
references:
  - 'https://developers.openai.com/cookbook/articles/sign-in-with-chatgpt'
  - 'https://developers.openai.com/siwc/token-sharing-open-source'
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 133000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Many users have a ChatGPT Plus or Pro plan and want to use it instead of paying for API usage. OpenAI's official Sign in with ChatGPT (DevDay, Sept 2026) allows plan usage for open-source and locally hosted apps, which harness is. The flow is an authorization-code flow with PKCE and a 127.0.0.1 redirect; OpenAI registers the client during the first sign-in using a persisted ext_agent_host_id, so no client secret is involved. OAuth tokens rotate, so they live in a protected token store rather than in the hand-edited harness.ini.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 provider.openai auth = api_key or chatgpt in harness.ini selects the method, and harness login openai offers both
- [x] #2 Sign-in opens the system browser, listens on 127.0.0.1 with fresh state, nonce and PKCE, verifies the ID token, and persists ext_agent_host_id and the issued client_id for reuse
- [x] #3 Sign-in requests offline_access and resource.invoke with chatgpt.tokens.use.direct, and fails clearly if plan usage was not granted, since identity alone does not allow it
- [x] #4 Tokens are stored in a 0600 file under data_dir that is a built-in protected path, refreshed automatically before expiry, and never written to events, logs or remote hosts
- [x] #5 Requests in chatgpt mode go to the Responses API with store false and streaming, and omit unsupported sampling parameters
- [x] #6 Hitting the plan usage limit shows an error whose primary action is Manage usage, with no silent fallback to an API key
- [x] #7 Headless and eval runs refuse chatgpt auth unless explicitly allowed, because the credential is personal and must not run in CI
- [x] #8 harness logout openai deletes the local tokens
- [x] #9 Tests use a local mock authorization server covering success, denied consent, missing plan scope, refresh and expired refresh token
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
1. harness-providers::chatgpt: constants/endpoints (auth.openai.com authorize/token/JWKS), PKCE/state/nonce/host id (aws-lc-rs rand+SHA256), RS256 ID-token verification against JWKS, 0600 token store under <data_dir>/auth, loopback SignIn flow, ChatGptAuth with serialized refresh, mock auth server + MockBrowser + TestSigner.
2. OpenAiProvider::with_chatgpt: per-call bearer from ChatGptAuth, body for_chatgpt() drops unsupported fields, store false + stream.
3. ProviderError::UsageLimit (+ action() = Manage usage) from HTTP bodies and response.failed; never retried.
4. harness-config: [provider.openai] auth = api_key|chatgpt and chatgpt_headless; DataDir::auth() in protected_paths; Credential::ChatGpt writes auth = chatgpt; LoginFlow Method::ChatGpt for openai (keyboard + click).
5. harness wiring: provider_sync picks ChatGPT mode, refuses headless unless chatgpt_headless and always in CI (TUI sets interactive in main.rs); catalog credentials; tokens added to redactor; login --chatgpt / menu option 4 / TUI apply; logout deletes tokens.
6. Tests with local mock server; docs (providers-openai, providers, data-dir, headless, doc-2, CHANGELOG).

7. Integration (AC6): harness-events ErrorAction {label,url} as an optional action on EventKind::Error; harness-agent StopReason::Error gains action filled from ProviderError::action() at the model-call failure site; the TUI driver opens a 'The agent stopped' panel whose first, focused button is Manage usage and opens the URL through an injectable Browser; fake provider gains FakeErrorKind::UsageLimit for offline tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented per plan. Endpoint and scope details taken from developers.openai.com/siwc/token-sharing-open-source (sign-in, errors-and-recovery, models-and-inference, preview-limitations, token-reference). JWKS URL assumed to be https://auth.openai.com/.well-known/jwks.json (not documented on those pages); refresh form sends grant_type, client_id, refresh_token, resource. No real ChatGPT account: verified only against chatgpt::mock::MockAuthServer (no live verification).
Decisions: tokens never in harness.ini, only auth = chatgpt; missing plan scope stores no tokens but keeps the issued client_id; dead refresh token clears tokens, keeps registration; network errors keep tokens; refreshes serialized per store dir via ChatGptAuth::shared and the store is re-read before refreshing (other processes). Headless refusal uses a process flag set only by cmd_tui (wiring::set_interactive), so run/wf/eval refuse unless chatgpt_headless = true, and CI env always refuses. Added aws-lc-rs (already the rustls crypto provider, same features) to workspace deps.
Tests: cargo test -p harness-providers --test chatgpt_sign_in (10: success incl. authorize/token params and 0600, denied, missing plan scope, forged state, timeout, refresh+rotation+Responses call, concurrent refresh, expired refresh token, temporary refresh failure, usage limit no-retry + Manage usage); unit tests in chatgpt::{pkce,jwt,store}, http, transport, openai::request; harness-config login/edit/config/paths tests; harness wiring::chatgpt_tests (headless/CI refusal, auth selection, catalog, redactor, policy denies token file) and login::chatgpt_tests (CLI --chatgpt, menu option 4, denied/missing scope change nothing, usage errors, TUI flow through apply, logout deletes tokens). cargo test --workspace: 1399 passed, 0 failed. clippy -D warnings clean for harness-providers, harness-config, harness.
OPEN (AC6, TUI part): the TUI shows the usage-limit error only as the 'the agent stopped' toast whose text contains 'Manage usage at https://chatgpt.com/settings/usage'. There is no clickable Manage usage button yet: harness-agent turns provider errors into StopReason::Error{message} (string), and toasts in harness-tui have no actions. Needs (outside the providers lane): carry ProviderError::action() through the agent loop/events and render it as the primary button in the TUI (tui lane). Also: the TUI driver awaits login::apply inline, so a pending ChatGPT sign-in (up to 5 min) blocks other core requests until it finishes or times out (tui wiring).

REVIEW 2026-10-22 (findings 1 and 2, both about AC6): verified and confirmed. Running grep -rn '\.action()\|UsageLimit\|ErrorAction' crates --include=*.rs outside harness-providers/src matches only tests/chatgpt_sign_in.rs:13,351-356. harness-agent wraps the provider error as StopReason::Error{message: format!("model call failed: {e}")} (agent_loop.rs ~970) and emits EventKind::Error{message, recoverable} (agent_loop.rs ~685). crates/harness/src/tui.rs ~451 shows it only as the 'the agent stopped' toast. No code was changed in this pass: the fix lies in the remote lane (harness-agent) and the tui lane (harness-tui, crates/harness/src/tui), I may not edit those, and ListAgents found no peer session to coordinate with. Hand-off plan: (a) harness-events adds an optional typed action {label, url} to EventKind::Error, or a new event; (b) harness-agent adds action: Option<..> to StopReason::Error, filled from ProviderError::action() at the model-call failure site; (c) the TUI renders the action as the primary registered button (click and Enter), opens the URL, and has tests for both input paths. Providers-lane note: ErrorAction.label is &'static str and not serde; change it to String plus Serialize if it goes into an event. AC6 and DoD #1 stay unchecked, and the status stays In Progress.

Integration 2026-10-22 (AC6, DoD#1): the action now flows provider -> agent loop -> event -> TUI. Changes: crates/harness-events/src/schema.rs (ErrorAction; EventKind::Error.action, serde default + skip when None, so v1 runs still load; covered by error_action_is_optional_on_the_wire and the round-trip test), redact.rs (label and url are redacted), harness-agent agent_loop.rs (StopReason::Error{message, action}; call_failed maps e.action(); the final error event carries it), harness-providers fake.rs (FakeErrorKind::UsageLimit -> ProviderError::UsageLimit with MANAGE_USAGE_URL), crates/harness/src/tui.rs + tui/commands.rs (error_action_panel, ERROR_ACTION button handled in panel_action, Driver.browser defaults to SystemBrowser). Evidence: harness-agent tests/agent_loop.rs usage_limit_stops_with_the_manage_usage_action (action in StopReason and in the error event; exactly one model call, so no retry or fallback); crates/harness tui::tests::usage_limit_offers_manage_usage_as_the_primary_button (driver plus a real harness_tui App on TestBackend: Manage usage is the first button, Enter activates it, a click activates it, the recording browser gets https://chatgpt.com/settings/usage each time, and the second scripted reply is never used). Docs: docs/providers-openai.md (Usage limit), docs/events.md, CHANGELOG. Workspace: cargo fmt --all --check clean, cargo clippy --workspace --all-targets -D warnings clean, cargo test --workspace 1411 passed / 0 failed / 8 ignored. Still known, not an AC: the TUI driver awaits login::apply inline, so a pending ChatGPT browser sign-in (up to 5 min) delays other core requests; the screen stays responsive. No live ChatGPT account was used; all verification is offline.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Sign in with ChatGPT for OpenAI: PKCE/state/nonce loopback sign-in with ID-token verification, a 0600 protected token store with serialized refresh, Responses API in chatgpt mode, headless/CI refusal, logout deleting tokens, and a usage limit that is never retried and never falls back to an API key. Integration closed AC6: the Manage usage action travels through StopReason::Error and the error event, and the TUI shows it as the first, focused panel button (Enter or click opens the usage page). Verified offline with chatgpt_sign_in mock-server tests, usage_limit_stops_with_the_manage_usage_action, usage_limit_offers_manage_usage_as_the_primary_button, and a green workspace (fmt, clippy -D warnings, 1411 tests). There was no live verification against a real ChatGPT account.
<!-- SECTION:FINAL_SUMMARY:END -->
