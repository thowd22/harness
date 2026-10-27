---
id: TASK-162
title: Browser sign-in over SSH and on headless machines (ChatGPT and MCP OAuth)
status: In Progress
assignee:
  - '@claude'
created_date: '2026-10-23 21:40'
updated_date: '2026-10-26 21:47'
labels:
  - auth
  - login
  - ssh
dependencies: []
priority: high
ordinal: 176000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Reported 2026-10-23: the user runs harness on a Linux box over SSH from Windows. 'harness login openai --chatgpt' tries to open a browser on the Linux box, not on Windows, and the sign-in callback listens on 127.0.0.1:1455 on the Linux box (crates/harness-providers/src/chatgpt.rs DEFAULT_CALLBACK_PORT; MCP OAuth uses a loopback redirect too, crates/harness-mcp/src/auth/flow.rs), so a Windows browser redirects to its own localhost and the code never reaches harness. The user also could not copy the sign-in URL out of the TUI in v0.3.0 (copy-on-select lands in the next release via TASK-149, but the login flow must not depend on it).

Make browser sign-in work when the browser is on another machine:
- Detect remote/headless sessions (SSH_CONNECTION/SSH_TTY, no DISPLAY/WAYLAND_DISPLAY, WSL) and do not try to open a local browser there; say so.
- Always show the sign-in URL in full (wrapped, never cut), as an OSC 8 hyperlink where supported, with a copy button (OSC 52) and a keybind, and print it plainly in the CLI flow so the terminal's own selection works.
- Provide ways to finish sign-in from another machine, in order of preference: (1) a device-code flow if the provider supports one for this client (check OpenAI's auth server and the Codex CLI for device authorization; verify, do not assume); (2) paste-back: after signing in, the user pastes the final redirect URL (or the code) into harness, which validates state and PKCE and completes the exchange; (3) documented SSH port forwarding (ssh -L 1455:127.0.0.1:1455 host) with the exact command shown when an SSH session is detected.
- Same treatment for MCP OAuth (harness login mcp <name>, /login mcp) and the TUI login modal.
- Security: keep state/PKCE checks, never log codes or tokens, time out abandoned flows, and keep tokens in the existing protected stores.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Over SSH (SSH_CONNECTION set, no DISPLAY) harness does not try to open a local browser and shows the full URL with copy and hyperlink affordances, in the CLI and the TUI modal
- [x] #2 Sign-in can be completed from a browser on another machine via paste-back of the redirect URL or code (and device-code if the provider supports it), with state and PKCE validated (tests against the existing mock auth servers)
- [x] #3 When SSH is detected the UI shows the exact ssh -L port-forward command as an alternative, and it works (test with a loopback forward)
- [x] #4 MCP OAuth gets the same remote-friendly flow; abandoned flows time out; codes and tokens never appear in logs or events
- [x] #5 docs (providers/login, mcp auth) describe signing in from a remote or headless machine, including Windows SSH clients
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
Research findings (2026-10-26):
- OpenAI device code: OpenAI's SIWC guide for open-source apps (developers.openai.com/siwc/token-sharing-open-source) documents only browser + loopback callback for the dynamic_agent_client registration. Codex's 'codex login --device-auth' uses a private deviceauth endpoint tied to Codex's own client id and a per-account toggle; harness must not borrow that client. So: no device code for ChatGPT.
- MCP: the MCP authorization spec defines authorization code + PKCE only; RFC 8628 is not part of it. Device code for MCP would be a separate follow-up (needs a device grant in DCR and server support); ask the user.
- TUI ChatGPT sign-in currently runs inline in the core driver (blocks it for up to 5 min) and never shows the URL; MCP shows the URL only in a toast.

Plan:
1. harness-providers::signin (shared by ChatGPT and MCP): detect remote sessions (SSH_CONNECTION/SSH_TTY, no DISPLAY/WAYLAND_DISPLAY on Linux, WSL; $BROWSER set = user choice wins), port-forward command built from SSH_CONNECTION and the redirect port, a parser for pasted redirect URLs, query strings or bare codes, a pre-check of a paste against the shown URL's state, and an OSC 8 hyperlink helper.
2. chatgpt::SignIn::run_with(browser, paste): wait for the loopback callback OR a pasted callback; same state, client_id and PKCE checks. Mock browser gets a 'remote' mode that captures the redirect without following it. Tests: paste URL, query, bare code, forged state, first registration without client_id, loopback-forward test (TCP proxy standing in for ssh -L), timeout.
3. harness-mcp Authorize::run_with: the same, including the RFC 9207 iss check on pasted callbacks. Tests against MockOAuthServer.
4. CLI (harness login openai --chatgpt, harness login mcp): no local browser when remote, URL printed plainly on its own line (OSC 8 when stderr is a tty), ssh -L command when SSH is detected, read pasted lines from stdin while waiting ('c' + Enter copies the link with OSC 52).
5. TUI: run ChatGPT and MCP sign-ins as background tasks; a sign-in modal shows the full wrapped URL as an OSC 8 link (drawn over the rendered cells after each draw), [copy link] (ctrl+y), [copy ssh command] when SSH, a paste field with [submit], and [cancel] (Esc aborts the flow). Tab/arrows move focus, Enter activates, every button is clickable. Tests for keyboard and mouse.
6. Docs: providers/login and MCP auth sections for remote/headless machines and Windows SSH clients; CHANGELOG.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Slices 1-4 done (2026-10-26):
- harness-providers/src/signin.rs: Environment (SSH/headless/WSL, $BROWSER override), port_forward_command, Instructions, parse_pasted, Paste/paste channel with per-paste answers, check_state, OSC 8 hyperlink.
- chatgpt::SignIn::run_with and harness-mcp Authorize::run_with take pasted callbacks; same state checks (and RFC 9207 iss for MCP); a bare code is accepted only when nothing else must be checked (ChatGPT: after registration; MCP: server without iss). A wrong paste is answered and the flow keeps waiting.
- Mocks: MockBrowser::remote() records the loopback redirect without following it.
- CLI: TerminalBrowser does not open a local browser when remote; drive_sign_in prints the URL alone on a line (OSC 8 on a tty), the ssh -L command over SSH, reads pasted lines from stdin (c copies via OSC 52).
- Tests: providers chatgpt_remote_sign_in.rs (5), harness-mcp oauth.rs (+5 incl. TRACE log capture), harness login tests (+3). Existing CLI tests now state a desktop env (DISPLAY) explicitly.

Slices 5-6 done (2026-10-26):
- TUI: harness-tui/src/signin.rs SignInModal (full link cut at the width, every row an OSC 8 Hyperlink written over the drawn cells after each frame by the run loop; address field shows only the part before '?'; [copy link]/ctrl+y, [copy ssh command], [submit], [cancel]; Tab/arrows/Enter/Esc via Interaction, every button clickable; a click outside does not cancel a running sign-in). App routes keys, pastes and mouse to it above the /login modal.
- Core: harness/src/tui/signin.rs runs ChatGPT and MCP sign-ins as background tasks (the driver used to await ChatGPT inline for up to 5 min), forwards pastes and answers, aborts on cancel (listener closes), applies the outcome (config reload for ChatGPT, toast + panel refresh for MCP). The MCP toast with the URL is replaced by the modal.
- CLI: when stdin is not a terminal the text no longer says 'paste it here'.
- Docs: providers.md 'Signing in from a remote or headless machine' (Windows OpenSSH and PuTTY tunnels, WSL), providers-openai.md, mcp.md, tui.md; CHANGELOG Unreleased/Added. No new config keys; no defaults changed.
- Device code: not offered (OpenAI documents only the loopback flow for this client; MCP spec has no device grant). Possible follow-up for MCP servers that advertise RFC 8628, not created.
- Flake found and fixed during the run: the TRACE log-capture test failed when run in parallel in the oauth binary (tracing callsite interest cached by other threads); moved to its own binary tests/oauth_no_leak.rs like harness-config's secrets_no_leak.rs.

Validation (local, 2026-10-26): cargo fmt --check ok; cargo clippy --workspace --all-targets -D warnings ok; cargo nextest run --workspace --locked --profile ci: 2095 passed, 10 skipped; cargo test --workspace --locked --doc ok; scripts/hermetic-guard.sh ok (265 passed); scripts/bench-guard.sh ok. Smoke: real binary 'harness login openai --chatgpt' with SSH_CONNECTION set and no DISPLAY prints the full link on its own line, the paste instructions and 'ssh -N -L 1455:127.0.0.1:1455 dev@10.0.0.2'. CI not run yet: nothing is committed or pushed (DoD #1 left unchecked until CI passes).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Browser sign-in (Sign in with ChatGPT, MCP OAuth) now works when the browser is on another machine. harness detects SSH/headless/WSL and does not open a local browser there; it shows the full link (OSC 8 in terminals, wrapped hyperlinked rows plus copy link/ctrl+y in the TUI, c to copy in the CLI), accepts the pasted redirect address or code with the same state/iss/PKCE checks as a real callback (wrong pastes are answered and the flow keeps waiting), and over SSH shows the exact ssh -L command. TUI sign-ins run in the background in a new keyboard- and mouse-operable sign-in dialog; Esc cancels and closes the listener. Device code is not offered (not supported for this OpenAI client; not in the MCP spec). Verified with new tests (providers chatgpt_remote_sign_in.rs incl. a TCP-forward test, harness-mcp oauth.rs + oauth_no_leak.rs TRACE capture, CLI SSH tests, TUI driver tests, modal/app/run-loop/layout-matrix tests) and locally: fmt, clippy -D warnings, nextest --profile ci (2095 passed), doctests, hermetic and bench guards; real-binary smoke over a simulated SSH session. CI pending until the change is pushed.
<!-- SECTION:FINAL_SUMMARY:END -->
