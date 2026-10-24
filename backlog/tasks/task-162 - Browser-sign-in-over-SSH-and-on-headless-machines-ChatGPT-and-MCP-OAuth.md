---
id: TASK-162
title: Browser sign-in over SSH and on headless machines (ChatGPT and MCP OAuth)
status: To Do
assignee: []
created_date: '2026-10-23 21:40'
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
- [ ] #1 Over SSH (SSH_CONNECTION set, no DISPLAY) harness does not try to open a local browser and shows the full URL with copy and hyperlink affordances, in the CLI and the TUI modal
- [ ] #2 Sign-in can be completed from a browser on another machine via paste-back of the redirect URL or code (and device-code if the provider supports it), with state and PKCE validated (tests against the existing mock auth servers)
- [ ] #3 When SSH is detected the UI shows the exact ssh -L port-forward command as an alternative, and it works (test with a loopback forward)
- [ ] #4 MCP OAuth gets the same remote-friendly flow; abandoned flows time out; codes and tokens never appear in logs or events
- [ ] #5 docs (providers/login, mcp auth) describe signing in from a remote or headless machine, including Windows SSH clients
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
