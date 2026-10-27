---
id: TASK-165
title: 'MCP OAuth: device authorization grant (RFC 8628) for servers that advertise it'
status: Done
assignee:
  - '@claude'
created_date: '2026-10-27 16:31'
updated_date: '2026-10-27 17:37'
labels:
  - auth
  - login
  - mcp
  - ssh
dependencies:
  - TASK-162
references:
  - 'https://www.rfc-editor.org/rfc/rfc8628'
  - 'https://www.rfc-editor.org/rfc/rfc8414'
documentation:
  - docs/mcp.md
priority: low
ordinal: 179000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Follow-up to TASK-162. Remote and headless MCP sign-in now works by pasting back the redirect address or by an `ssh -L` port forward, but both need the user to move text or a tunnel between machines. Some authorization servers publish a `device_authorization_endpoint` in their RFC 8414 metadata; for them, harness could show a short user code and verification URL that the user opens on any device, with no loopback redirect at all. The MCP authorization spec does not require or mention the device grant, so this is an optional path for servers that support it, never a replacement for the loopback flow. ChatGPT sign-in is out of scope: OpenAI documents only the loopback flow for this kind of client, and the Codex CLI device login uses the Codex client id (see TASK-162 notes).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 When the authorization server metadata advertises `device_authorization_endpoint` and the session is remote or headless, `harness login mcp <name>` and `/login mcp <name>` offer the device flow and show the user code and verification URL (full, copyable, hyperlinked)
- [x] #2 Polling honours `interval`, `slow_down`, `authorization_pending`, `access_denied` and `expired_token` per RFC 8628, and the flow times out and can be cancelled (Esc in the TUI)
- [x] #3 Servers without the endpoint keep the loopback, paste-back and port-forward flow unchanged
- [x] #4 Tokens go to the existing MCP token store; device codes, user codes and tokens never reach logs (TRACE leak test)
- [x] #5 Offline tests against the mock OAuth server cover success, slow_down, denial, expiry and cancel; docs/mcp.md describes when the device flow is used
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
Branch task-165-mcp-device-grant from task-162-remote-sign-in.

Decisions:
- The device grant is used only when (a) the session is remote/headless (Environment::is_remote(): SSH, no display, WSL) and (b) the RFC 8414 metadata advertises device_authorization_endpoint. Otherwise the TASK-162 loopback + paste-back + ssh -L flow runs exactly as before (no change to DCR body, URLs or UI).
- Fallback: no paste-back while a device code is shown (there is no redirect to paste). If the device authorization request itself is refused with an OAuth error (e.g. unauthorized_client for a pre-registered or older dynamic client), harness falls back automatically to the loopback + paste-back flow in the same run. Network/5xx errors fail the run.
- PKCE does not apply to RFC 8628 (no authorization code); the device request and every poll carry client_id, resource (RFC 8707), scope (request only) and client_secret when DCR issued one. DCR registers urn:ietf:params:oauth:grant-type:device_code in grant_types only when the device flow will be used.
- Polling: wait interval (default 5s, min 1s) before each poll; authorization_pending keeps polling, slow_down adds 5s, access_denied -> Denied, expired_token -> new DeviceExpired error; deadline = min(flow timeout, expires_in). Cancellation = dropping/aborting the future (TUI Esc aborts the task; CLI Ctrl+C). A poll_unit knob (Authorize::with_poll_unit, default 1s) lets tests run intervals in milliseconds.
- device_code is Zeroizing and never logged; user_code and verification_uri_complete are shown to the user only; DeviceCode's Debug hides them.

Steps:
1. harness-providers::signin: DeviceCode (user_code, verification_uri, verification_uri_complete, expires_in; redacted Debug; link(); terminal lines). Browser::show_device_code default method (opens the link).
2. harness-mcp: AuthServer.device_authorization_endpoint (https check); auth/device.rs with request + poll loop; Authorize.device_grant/with_device_grant/with_poll_unit; AuthError::DeviceExpired; fallback on refusal.
3. Mock: advertise device endpoint (MockConfig.device_authorization), /device_authorization, /device approval page (GET with user_code approves or denies), scripted poll answers (slow_down/pending/expired), interval/expires_in/refuse/no-complete knobs; request timestamps for interval checks.
4. Tests (harness-mcp tests/oauth.rs): success + tokens in store, slow_down/pending intervals, denial, expiry, timeout, cancel (dropped future, no more polls), fallback on refusal, desktop or no endpoint keeps loopback; oauth_no_leak.rs TRACE test for device code/user code/tokens.
5. CLI: drive_sign_in shows device codes (link on its own line, OSC 8, code on its own line, c copies link, u copies code), authorize() takes the device preference from Environment::is_remote(); tests.
6. TUI: SignInPrompt.device; modal device mode (link rows + code, [copy link] [copy code] [cancel], c copies code, ctrl+y link, Esc cancels; no address field); ModalBrowser::show_device_code; keyboard+mouse modal tests, app test, driver tests (device success over SSH, Esc cancel stops polling).
7. Docs: docs/mcp.md (when the device flow is used), harness-providers signin module docs, CHANGELOG Unreleased. Verify fmt, clippy, nextest ci, doctests, hermetic and bench guards.

Revision during implementation: ClientRegistration gains device_grant (serde default false, omitted when false) so a loopback-only dynamic registration from before is replaced once when the device flow is used, instead of being refused and always falling back.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Slices 1-5 done (2026-10-27):
- harness-providers::signin::DeviceCode (user code + verification URIs, Zeroizing, Debug hides the code; link(), lines()); Browser::show_device_code default method (opens the link).
- harness-mcp: AuthServer.device_authorization_endpoint (https check like the other endpoints); auth/device.rs (request + poll loop: interval default 5 min 1, slow_down +5, authorization_pending, access_denied -> Denied, expired_token -> new AuthError::DeviceExpired, deadline min(expires_in, timeout), network errors back off x2); Authorize::with_device_grant / with_poll_unit; DCR adds the device grant type only when the device flow is used; ClientRegistration.device_grant (serde default false) so an older loopback-only dynamic registration is replaced once for the device flow; refusal of the device request (4xx OAuth error) falls back to the loopback + paste flow in the same run.
- Mock: device endpoint, /device verification page (visiting approves or, with deny, declines), scripted poll errors, interval/expires_in/complete/refuse knobs, request timestamps.
- Tests: tests/oauth.rs +8 (success, interval/pending/slow_down gaps + default 5, denial, expiry by server/by expires_in/timeout, cancel stops polling, no endpoint/desktop unchanged + paste-back, refusal fallback, re-registration); oauth_no_leak.rs + device TRACE test. All pass first try.
- CLI: TerminalBrowser sends Shown::Url|Device; drive_sign_in prints the link (OSC 8) and code on lines of their own, c copies link, u copies code, other typed text is answered 'Nothing to paste'; authorize() takes device_grant = Environment::is_remote(). Tests: over_ssh_a_device_code_is_shown_and_approving_it_authorizes, on_a_desktop_a_device_capable_server_keeps_the_browser_redirect.

Slice 6-7 done (2026-10-27):
- TUI: SignInPrompt.device (Option<DeviceCode>), manual Debug that drops the URL query (state / user code). SignInModal device mode: link rows (OSC 8, per-row targets now), Code <user_code>, the plain verification page as a second link when verification_uri_complete is given, expiry line; [ copy link ] (ctrl+y), [ copy code ] (c), [ cancel ] (Esc); no address field, typing/pasting ignored. ModalBrowser::show_device_code opens the modal (forward None). Layout matrix covers the device dialog.
- Tests: harness-tui signin a_device_code_shows_the_link_and_the_code_without_an_address_field, device_keyboard_path_copies_and_cancels, device_mouse_path_clicks_every_button, app_device_sign_in_copies_the_code_and_cancels_by_key_or_mouse; driver mcp_over_ssh_shows_a_device_code_and_approving_it_authorizes, cancelling_a_device_sign_in_stops_polling.
- Docs: docs/mcp.md 'Signing in with a code (device flow)' (when it is used, what is shown, polling, fallback), providers.md device paragraph updated, harness-providers::signin and harness-mcp::auth module docs, login mcp USAGE, CHANGELOG Unreleased/Added. No new config keys, no default changed.
- Fallback decision: no paste-back while a code is shown (nothing to paste; typed text in the CLI is answered 'Nothing to paste'); automatic fallback to loopback + paste + ssh -L only when the device authorization request is refused with an OAuth 4xx error.

Validation (local, 2026-10-27): cargo fmt --check ok; cargo clippy --workspace --all-targets -- -D warnings ok; cargo nextest run --workspace --locked --profile ci: 2113 passed, 10 skipped; cargo test --workspace --locked --doc ok; scripts/hermetic-guard.sh ok (269 passed); scripts/bench-guard.sh ok; cargo doc --no-deps for harness-mcp/providers/tui without warnings. No fix attempts were needed (every new test passed on its first run). CI not run yet (DoD #1 left unchecked).

CI green on PR #4 (https://github.com/thowd22/harness/pull/4, run 40551316751, commit 78fa097, shared branch with TASK-162 and the other task): rustfmt, cargo-deny, clippy and test on ubuntu-latest and macos-latest all pass.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
MCP OAuth now uses the device authorization grant (RFC 8628) when harness runs over SSH, without a display or in WSL and the authorization server advertises device_authorization_endpoint: harness login mcp <name> and /login mcp <name> show the link (OSC 8, verification_uri_complete when given, plus the plain page) and the user code on lines of their own, with copy (CLI c/u via OSC 52; TUI [ copy link ]/ctrl+y, [ copy code ]/c), then poll the token endpoint honouring interval (default 5), slow_down (+5), authorization_pending, access_denied, expired_token/expires_in and the 5-minute timeout; Esc/[ cancel ] (TUI) or Ctrl+C stops polling. Tokens go to the existing MCP token store. Desktop sessions and servers without the endpoint keep the loopback, paste-back and ssh -L flow unchanged; a server that refuses the device grant falls back to it in the same run. Device codes, user codes and tokens never reach logs or events. Verified with new offline tests against the mock OAuth server (harness-mcp tests/oauth.rs: success, interval/slow_down gaps, denial, expiry, timeout, cancel, unchanged loopback, fallback, re-registration; oauth_no_leak.rs TRACE device test), CLI tests (SSH device flow, desktop keeps redirect), TUI modal keyboard/mouse/app tests and driver tests (success over SSH, cancel stops polling), and locally: fmt, clippy -D warnings, nextest --profile ci (2113 passed), doctests, hermetic and bench guards. CI green on PR #4 (run 40551316751).
<!-- SECTION:FINAL_SUMMARY:END -->
