---
id: TASK-146.3
title: MCP auth for remote servers (OAuth)
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 06:01'
updated_date: '2026-10-23 10:18'
labels:
  - mcp
  - security
  - problem-8
milestone: m-2
dependencies:
  - TASK-146.1
  - TASK-133
references:
  - 'https://modelcontextprotocol.io/specification'
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
parent_task_id: TASK-146
priority: medium
type: feature
ordinal: 149000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Remote MCP servers increasingly require OAuth. Tokens are personal credentials and must be stored and refreshed as safely as the ChatGPT sign-in tokens.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Remote servers that require authorization are supported via the OAuth flow defined by the MCP specification (authorization code with PKCE, loopback redirect, dynamic client registration where the server supports it)
- [x] #2 harness login mcp <name> and a /login palette action run the flow; tokens are stored in a 0600 file under data_dir that is a built-in protected path, refreshed automatically, never logged or sent to remote hosts
- [x] #3 Static bearer tokens or API keys via headers keep working with the standard secret resolution
- [x] #4 Headless and eval runs refuse interactive OAuth and report how to authorize first
- [x] #5 Tests use a local mock authorization server: success, denied consent, refresh, expired refresh token and revoked token
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
1. harness-config: [mcp.<name>] keys auth = none|oauth (HTTP only), oauth_client_id, oauth_client_metadata_url, oauth_scopes; DataDir::mcp_auth() = <data_dir>/auth/mcp (inside the protected auth dir); edit::save_section_key to set auth = oauth after login.
2. harness-mcp auth module: store (0600 per-server JSON, 0700 dir, Token type reused from chatgpt store, record bound to resource URL + issuer), discovery (WWW-Authenticate parsing, RFC 9728 PRM with resource check, RFC 8414/OIDC metadata in spec order with issuer check, S256 required), registration (pre-registered > CIMD > DCR, DCR client reused per issuer), authorize (PKCE S256, state, resource, iss check per RFC 9207, loopback redirect on 127.0.0.1), session (shared per store dir+server, serialized refresh with rotation, re-read store, dead refresh token clears tokens), mock (local AS + protected MCP proxy over test_server, MockBrowser).
3. HttpTransport sends Authorization: Bearer from the session on every request; on 401 refreshes once and retries; 403 insufficient_scope records step-up scopes; no tokens -> McpError::AuthRequired telling to run harness login mcp <name> (never opens a browser outside login, so headless/eval refuse). 401 without auth = oauth hints at auth = oauth.
4. ClientOptions gains auth_dir (from_config fills it) and stored MCP tokens join known_secrets; wiring::credentials adds them to the redactor.
5. harness: harness login mcp <name> / harness logout mcp <name>; TUI /login mcp <name> plus a palette entry per OAuth server, flow runs in the background with toasts for URL and outcome.
6. Tests with the mock AS: success (DCR + pre-registered), denied consent, refresh, expired refresh token, revoked token, headless refusal, 0600 + protected path, static header tokens unchanged; TUI palette/slash tests.
7. Docs: docs/mcp.md (Authorization), doc-2, docs/cli.md, CHANGELOG.

5. Review fix: bearer tokens only over TLS. auth = oauth needs an https url (http only on loopback) as a load error, refused in Authorize::run, OAuthSession and HttpTransport::token; PRM discovery requires secure_endpoint even same-origin.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented: harness-config [mcp.<name>] auth = none|oauth, oauth_client_id, oauth_client_metadata_url, oauth_scopes (HTTP only; oauth_* need auth = oauth; auth = oauth + header.Authorization is a load error), DataDir::mcp_auth (<data_dir>/auth/mcp, inside the protected auth dir), edit::save_section_key. harness-mcp auth module: discovery (WWW-Authenticate parser, RFC 9728 PRM from resource_metadata or well-known path-insert/root with resource check, RFC 8414/OIDC metadata in spec order with exact issuer check, S256 required, https or loopback-http endpoints only), flow::Authorize (pre-registered > CIMD > DCR with application_type native and token_endpoint_auth_method none; DCR reused per issuer; PKCE S256, state, resource, iss check per RFC 9207 before using the code; loopback 127.0.0.1 redirect /callback), store (0600 file per server, 0700 dirs, Token type from the ChatGPT store, bound to resource URL and issuer), session::OAuthSession (process-wide per store dir+server, serialized refresh with rotation, re-reads store, refresh once after 401, terminal codes clear tokens and keep registration), mock (AS + protected MCP proxy over test_server + MockBrowser). HttpTransport sends Bearer on every request (also notify, legacy replies, DELETE), maps 401-with-challenge without auth=oauth to McpError::Auth naming harness login mcp, 403 insufficient_scope records step-up scopes. ClientOptions::auth_dir (from_config fills it, stored tokens join known_secrets); wiring::credentials adds MCP tokens; doctor fix hint for Auth errors. harness: login mcp/logout mcp (crates/harness/src/login/mcp.rs), TUI /login mcp [server] panel with authorize buttons, palette entries mcp.login.<name>, flow in a background task with URL and result toasts (crates/harness/src/tui/mcp_login.rs); /login arg spec gains mcp + server. Decisions: confidential pre-registered clients (client secret in ini) and client_credentials are not supported (documented); DCR-issued client_secret is stored and sent. Note: cargo fmt --all also reformatted crates/harness-mcp/src/client.rs and test_server.rs (formatting only, files owned by other tasks).
Validation: cargo nextest run --workspace 1655 passed / 0 failed; harness-mcp tests/oauth.rs 13 tests (dynamic + pre-registered success, denied, refresh+rotation, expired refresh token, revoked access token and revoked grant, concurrent refresh once, headless refusal incl. doctor hint, OAuth server without auth=oauth, static bearer header, well-known fallback, issuer mismatches, step-up scope); unit tests in auth::{discovery,store}, ini mcp/edit, paths; harness login::mcp::tests (CLI login sets auth = oauth, 0600, protected path, redactor, logout), tui::tests::login_mcp_authorizes_from_slash_palette_and_panel_buttons (Enter and click on the real App, palette entry, /login mcp docs, no token in UI events). cargo clippy --workspace --all-targets -D warnings clean, rustdoc -D warnings clean for harness-mcp, doctests pass. Docs: docs/mcp.md (Authorization section, keys, troubleshooting), doc-2 via backlog doc update, docs/cli.md, docs/commands.md, docs/data-dir.md, CHANGELOG.

Review fix (cleartext bearer tokens), both blocking findings confirmed and fixed: (1) harness-config check: auth = oauth with an http:// url on a host other than 127.0.0.1/localhost/[::1] is a load error on the url line (loopback_http helper, userinfo/port aware); test ini::mcp::tests::oauth_needs_tls_except_on_loopback. (2) harness-mcp: new AuthError::InsecureUrl; Authorize::run refuses before the probe (no request, no browser, nothing stored); OAuthSession::access_token/record refuse before reading or refreshing tokens; HttpTransport::token refuses before attaching Authorization to POST, GET stream and DELETE (refresh_after_rejection only runs after token() succeeded). discovery::protected_resource now skips any PRM URL failing secure_endpoint, including same-origin (same_origin helper removed). Integration test tests/oauth.rs oauth_refuses_cleartext_http_to_remote_hosts (http://mcp.example.invalid: login, session with stored tokens, ServerHandle list_tools, PRM via well-known and via challenge). Docs: docs/mcp.md (load errors, discovery, troubleshooting row), doc-2 via backlog doc update, CHANGELOG Unreleased OAuth entry. Validation: cargo nextest -p harness-mcp 82/82, -p harness-config 165/165, -p harness mcp/login/oauth filter 19/19, clippy -p harness-mcp -p harness-config --all-targets -D warnings clean, rustdoc -D warnings clean. (cargo fmt --check reports a diff in crates/harness-tui/src/cards.rs owned by another task; not touched.)
<!-- SECTION:NOTES:END -->

## Comments

<!-- COMMENTS:BEGIN -->
created: 2026-10-23 10:18
---
Shipped in v0.3.0: https://github.com/thowd22/harness/releases/tag/v0.3.0 (commits fcbd379, 74cb913; CI https://github.com/thowd22/harness/actions/runs/39934504416). Installed locally via install.sh: harness 0.3.0 (2cb6cd9e4cd6).
---
<!-- COMMENTS:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
OAuth for remote MCP servers per the MCP authorization spec: auth = oauth in [mcp.<name>] (plus oauth_client_id, oauth_client_metadata_url, oauth_scopes), discovery (RFC 9728/8414/OIDC with resource and issuer checks, S256 required), pre-registered/CIMD/dynamic registration, PKCE + state + resource + iss-checked loopback flow via harness login mcp <name>, /login mcp [name] (panel buttons, palette entries, background flow with toasts) and harness logout mcp. Tokens in <data_dir>/auth/mcp/<name>.json (0600, protected path, bound to the server URL), Bearer on every request and only over TLS (auth = oauth with a non-loopback http:// url is a load error and is refused by login, the session, the transport and PRM discovery), automatic refresh with rotation and serialized refresh, dead refresh tokens cleared with a re-authorize message; runs never open a browser and name the login command; static header tokens unchanged. Verified offline with the mock authorization server (harness-mcp tests/oauth.rs, 14 tests incl. cleartext refusal), config unit tests, login::mcp and TUI tests (keyboard and mouse), nextest/clippy/rustdoc clean for touched crates.
<!-- SECTION:FINAL_SUMMARY:END -->
