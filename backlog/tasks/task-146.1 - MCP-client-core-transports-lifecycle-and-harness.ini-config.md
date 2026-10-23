---
id: TASK-146.1
title: 'MCP client core: transports, lifecycle and harness.ini config'
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 06:01'
updated_date: '2026-10-23 10:18'
labels:
  - tools
  - mcp
  - config
milestone: m-2
dependencies:
  - TASK-2
  - TASK-3
  - TASK-20
references:
  - 'https://modelcontextprotocol.io/specification'
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
parent_task_id: TASK-146
priority: high
type: feature
ordinal: 147000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Foundation for MCP: connecting to servers and keeping them healthy. Servers are user-granted, so they are defined only in harness.ini; project YAML can reference them but never add one.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 mcp.<name> sections in harness.ini define stdio servers (command, args, env, cwd) or remote servers (url, headers), with enabled, timeout and startup settings; secrets in env or headers use the same api_key/_cmd/_env resolution and redaction as provider keys
- [x] #2 The client implements the MCP lifecycle (initialize, capability negotiation, ping, shutdown) and JSON-RPC over stdio and streamable HTTP, following the current specification
- [x] #3 Stdio servers start lazily on first use, run in their own process group and session, are restarted with backoff on crash, and are always cleaned up on exit (no orphans), using killpg, never kill with a negative id
- [x] #4 Server stderr is captured to the log (redacted), not the terminal; slow or hung servers time out per request
- [x] #5 harness doctor checks each configured server (starts, initializes, lists tools) and reports actionable errors
- [x] #6 Tests use a bundled minimal MCP test server for both transports, including crash, timeout and malformed-message cases
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
1. Config: new harness-config ini/mcp.rs with [mcp.<name>] (command/args/cwd/env.* or url/header.*, enabled, timeout, startup, startup_timeout); env/header values are McpSecret with inline/_cmd/_env resolution; wire into Config (mcp map, has_inline_secrets, secret_env_vars, config show rows).
2. New crate crates/harness-mcp: protocol (2026-07-28 modern _meta + legacy initialize, error codes, resultType), stdio transport (setsid group, killpg, redacted stderr to log, bounded lines, malformed-line tolerance, cancel on timeout), streamable HTTP transport (JSON/SSE, MCP-Protocol-Version/Mcp-Method/Mcp-Name/Mcp-Param-* headers, legacy sessions + DELETE), client (dual-era detection per spec, ping, tools/list pagination, tools/call), server handle (lazy start, restart with exponential backoff, give-up, eager start, shutdown, kill_all), doctor check.
3. Bundled test server (lib module + harness-mcp-test-server bin, in-process HTTP server) supporting modern/legacy/dual, SSE, and misbehaving tools.
4. harness doctor: Probes::mcp + mcp checks in harness-eval.
5. Tests for every AC; docs (docs/mcp.md, config.md, doctor.md, README, CHANGELOG, doc-1, doc-2).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Spec: read https://modelcontextprotocol.io/specification (current 2026-07-28). It removed initialize/ping/sessions in favour of per-request _meta and server/discover, so the client is dual-era: modern by default, legacy initialize (2025-11-25/06-18/03-26) fallback per the spec's stdio probe and HTTP 4xx rules. 'ping' uses ping on legacy servers and server/discover on modern ones. Shutdown: stdio close stdin -> SIGTERM -> SIGKILL to the group; HTTP legacy DELETE session.
Decisions: env.*/header.* values are all credentials (Secret, redacted, inline ones count for the 0600 rule); keys ending _cmd/_env always name a source. Stdio servers get provider api_key_env vars and *_env source vars scrubbed (ClientOptions::from_config). Crashed requests are not retried (tool calls may have side effects); next use restarts after backoff 0.5s doubling to 30s, give up after 5 consecutive failures. Config.mcp is skipped in serialization when empty so existing config hashes do not change.
Tests: crates/harness-mcp/tests/{stdio,http,config_and_logs,stderr_log,kill_all}.rs (kill_all and the tracing-capture test are in their own binaries because they are process-global), harness-config ini::mcp + effective tests, harness-eval doctor::checks_each_mcp_server. Validation: cargo nextest run -p harness-mcp -p harness-config -p harness-eval -p harness: 383 passed; cargo test -p harness-mcp passes (repeated 3x); clippy -D warnings clean; fmt clean; scripts/hermetic-guard.sh nextest run -p harness-mcp -p harness-config: ok, nothing written to HOME/XDG; no leftover test-server or grandchild processes after runs.
Not wired yet: the harness binary does not create Servers or call kill_all at exit (no MCP tools reach agents until TASK-146.2); wiring::credentials does not include MCP secrets (the MCP client uses its own per-server redactor).

DoD #4 UI interaction: N/A, this task adds no TUI element (the /mcp UI is TASK-146.4).

Review fixes (round 1):
- Findings 1+3 (same bug, confirmed): Drop/shutdown SIGKILLed self.pgid even after wait_child had reaped the leader, so a reused pid's group could be hit. Fix: per-transport Group guard in stdio.rs (pgid + reaped flag under a mutex). wait_child marks it reaped (killing leftover children once) right after the reap; shutdown/Drop/escalation signal and untrack only while unreaped, so a stale pgid is never signalled and a reused pid tracked by a newer transport is never untracked. Remaining unguarded window is the instant between the reap inside tokio's wait and taking the lock (documented). Unit tests in stdio.rs record every killpg under cfg(test): no_signal_after_the_leader_was_reaped, graceful_shutdown_then_drop_signals_nothing_extra, dropping_a_running_server_kills_its_group.
- Finding 2 (confirmed): reqwest default redirect policy would forward custom header.* credentials and the body cross-origin. Fix: Policy::none(); a 3xx is an McpError::Http naming the redirect target and telling the user to set url to the final endpoint. Test tests/http.rs redirects_are_not_followed_so_credentials_stay_put (302/307/308, target never receives a request); verified it fails with the policy line removed.
- Finding 4 (confirmed): stdio spawn now applies harness_tools::bash::EnvScrubber (DEFAULT_SECRET_NAMES, HARNESS_*, configured names, value matching on known_secrets) before adding the server's env.*; harness-mcp depends on harness-tools. ClientOptions::from_config now resolves provider keys into known_secrets (redactor + value scrubbing); doctor computes it once. Tests: stdio.rs environment_uses_the_bash_scrub_policy and tests/env_scrub.rs (re-runs itself with ANTHROPIC_API_KEY, HARNESS_*, a copy of a resolved key etc. in the inherited env, with HOME/XDG/HARNESS_CONFIG at a tempdir).
Docs: docs/mcp.md, doc-2, CHANGELOG. Validation: cargo nextest run -p harness-mcp 44/44 (3x), -p harness-eval -p harness-config 215 passed; clippy -D warnings and fmt clean for harness-mcp/harness-eval; harness builds; hermetic-guard nextest -p harness-mcp ok; no leftover test-server or sleep processes.
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
Added the MCP client core. harness-config gains [mcp.<name>] sections (stdio: command/args/cwd/env.*; HTTP: url/header.*; enabled, timeout, startup, startup_timeout) with env/header credentials resolved like api_key (_cmd/_env), redacted in config show and logs, counted for the 0600 rule. New crate crates/harness-mcp: stdio and streamable HTTP transports (JSON and SSE), dual-era lifecycle per MCP 2026-07-28 (server/discover + per-request _meta, falling back to the legacy initialize handshake with capability negotiation, ping, shutdown/DELETE), lazy start, own session/process group via setsid, restart with exponential backoff and give-up, killpg-only cleanup on shutdown/drop/kill_all, redacted stderr to the log (target harness_mcp::stderr), per-request timeouts with cancellation, tolerance of malformed output, x-mcp-header mirroring. Bundled test server (harness-mcp-test-server binary + in-process HTTP server, modern/legacy/dual, crash/timeout/malformed tools). harness doctor checks each server (start, handshake, list tools) with fixes. Docs: docs/mcp.md, config.md, doctor.md, README, CHANGELOG, doc-1, doc-2. Verified with 38 harness-mcp tests over both transports plus config/effective/doctor tests; nextest for harness-mcp/config/eval/harness all green (383), clippy and fmt clean, hermetic guard ok, no orphan processes.
<!-- SECTION:FINAL_SUMMARY:END -->
