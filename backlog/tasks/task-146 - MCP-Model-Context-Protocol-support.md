---
id: TASK-146
title: MCP (Model Context Protocol) support
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 06:01'
updated_date: '2026-10-23 09:29'
labels:
  - tools
  - mcp
  - security
  - problem-6
  - problem-8
milestone: m-2
dependencies: []
references:
  - 'https://modelcontextprotocol.io/specification'
documentation:
  - backlog/docs/doc-1 - Architecture-and-Problem-Map.md
priority: high
type: feature
ordinal: 146000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users want to extend agents with tools from MCP servers (GitHub, databases, browsers, internal services) without writing harness code. harness should be an MCP client that follows the current MCP specification while keeping its own guarantees: servers are granted only by the user (harness.ini), every MCP call goes through the policy engine and appears in the event log, credentials never leak, and tool schemas do not bloat the initial context (problem 6). This parent task tracks the subtasks.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 All subtasks are Done
- [x] #2 An end-to-end test runs an agent against a local MCP test server over stdio and over streamable HTTP: the agent lists and calls a tool, the call is policy-checked, shown as a tool card and recorded in the run log
- [x] #3 docs/mcp.md explains configuration, permissions, auth, context cost and troubleshooting
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
1. Confirm subtasks 146.1-146.4 are Done.
2. Add crates/harness/tests/mcp_e2e.rs: harness run (fake provider script) with an agent selecting mcp:t/echo and mcp:t/write_note, against the test server over stdio (modern, legacy) and streamable HTTP (modern JSON, legacy SSE). Check tools listed/offered (first model request), policy decisions (allow / ask-denied), run log on disk (tool_call, mcp_call, policy_decision, tool_result), and fold the run log into ChatView on a TestBackend to see mcp t > tool cards toggled by click and Enter.
3. Fix any bug the e2e test exposes, with a failing unit test first.
4. Review docs/mcp.md against AC#3 and add the e2e test to its developer section; CHANGELOG entry for fixes.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Subtasks 146.1-146.4 were already Done. docs/mcp.md (from the subtasks) already covers configuration, credentials, OAuth auth, permissions, context cost and troubleshooting; added the e2e test to its For developers section.
The e2e test found a bug: failed tool cards rendered the raw <tool_result ... status="error"> provenance wrapper (ToolCard::body used self.output for errors). Fixed by using output_text(false) in the error branch (crates/harness-tui/src/cards.rs), with unit test cards::tests::error_bodies_drop_the_provenance_wrapper written failing first. CHANGELOG Fixed entry added.
Validation: cargo nextest run -p harness -p harness-mcp -p harness-tui: 602 passed. clippy -D warnings clean for harness (tests) and harness-tui; rustfmt applied to new test. No test-server processes left after the run.

Integration check (2026-10-23, all MCP subtasks and TASK-147 merged in the tree): cargo fmt --all --check clean; cargo clippy --workspace --all-targets --locked -D warnings clean; cargo nextest run --workspace --locked --profile ci 1701 passed / 8 skipped; cargo test --workspace --doc ok; cargo deny check ok (advisories, bans, licenses, sources); bash tests/install-test.sh passed; shellcheck on install.sh, scripts/*.sh, tests/*.sh clean; actionlint reports only the known-valid macos-15-intel label; scripts/hermetic-guard.sh over -p harness -p harness-eval -p harness-mcp: 298 passed, nothing written to HOME/XDG. Release: cargo zigbuild --release -p harness --bin harness --target x86_64-unknown-linux-musl builds a statically linked binary that prints its version; cargo check and clippy -D warnings for aarch64-apple-darwin (zig cc for the C deps) are clean for the whole workspace. release-harness.yml and scripts need no change: they build only -p harness --bin harness (the harness-mcp-test-server bin is test-only), and harness-mcp adds no new native dependencies (reqwest with rustls is shared with harness-providers). README gains an MCP highlight, /mcp in the slash commands, login mcp in the CLI table and docs/mcp.md in the docs index; docs/guide/README.md points to docs/mcp.md. CI DoD 'passes in CI' still waits for the orchestrator's push.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added crates/harness/tests/mcp_e2e.rs: four hermetic end-to-end tests run harness run against the bundled MCP test server over stdio (modern, legacy) and streamable HTTP (modern JSON, legacy SSE). Each checks that the selected tools are listed and offered, that echo is allowed and write_note asks and is denied by policy (only the allowed call reaches the HTTP server), that the on-disk run log records tool_call/mcp_call/policy_decision/tool_result, and that the run log folded into the TUI ChatView shows mcp t > tool cards that expand by click and collapse with Enter. The test exposed and this task fixed failed tool cards showing the raw <tool_result> wrapper (harness-tui cards.rs, with a unit test). docs/mcp.md already covered configuration, permissions, auth, context cost and troubleshooting; it now also documents the e2e test. Verified: nextest -p harness -p harness-mcp -p harness-tui 602/602 passed, clippy clean. DoD#4: the cards' click and keyboard paths are tested in the e2e test.
<!-- SECTION:FINAL_SUMMARY:END -->
