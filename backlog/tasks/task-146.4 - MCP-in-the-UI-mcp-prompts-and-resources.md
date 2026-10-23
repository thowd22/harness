---
id: TASK-146.4
title: 'MCP in the UI: /mcp, prompts and resources'
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 06:01'
updated_date: '2026-10-23 09:16'
labels:
  - tui
  - mcp
  - commands
milestone: m-4
dependencies:
  - TASK-146.1
  - TASK-146.2
  - TASK-36
  - TASK-45
references:
  - 'https://modelcontextprotocol.io/specification'
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
parent_task_id: TASK-146
priority: medium
type: feature
ordinal: 150000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users need to see which servers are connected and what they offer, and MCP prompts and resources should be usable without leaving the TUI.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 /mcp opens a panel listing servers with status (connected, starting, failed with reason, disabled), transport, tool count and auth state, with actions to restart, disable for the session or view logs; everything clickable and keyboard-operable
- [x] #2 MCP prompts appear as slash commands namespaced by server (e.g. /github:review-pr) with their arguments, and in the palette
- [x] #3 MCP resources can be attached to a message from the palette (search by server and name) and are labeled as untrusted content
- [x] #4 The status line shows a compact MCP indicator when any server has failed
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
1. harness-mcp: prompts/resources protocol module (prompts.rs), Connection + ServerHandle list/get/read (Mcp-Name on modern HTTP), Status::Starting/DisabledForSession, status watch, disable/enable for session, restart; test server offers prompts and resources.
2. harness-policy: Source::McpResource label (web_content tag, origin=mcp server, always untrusted).
3. harness-agent: /mcp builtin.
4. harness-tui: EntryKind::Mcp, CoreEvent::McpStatus, status-line indicator registered as a button (opens /mcp).
5. harness TUI driver (tui/mcp.rs): /mcp panel with restart/disable/enable/logs buttons, background watcher refreshing panel text, palette, slash completion and indicator; /<server>:<prompt> commands with argument binding and palette prompting; resource attachments prepended to the next message.
6. Tests per AC; docs (mcp.md, commands.md, tui-views.md, guide/palette.md, doc-1, CHANGELOG).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Design: lazy servers stay lazy. Prompts/resources are listed only for running servers (eager, after use, or via the restart button); typing /<server>:<prompt> for a not-yet-started server starts it and looks the prompt up. ServerHandle::status never waits on a start (Starting flag + 200 ms lock timeout). Attached resources and embedded prompt resources are labeled with the new Source::McpResource (reuses the web_content tag so the system prompt and golden request snapshots are unchanged). The status-line indicator is focusable via Shift+Tab from the input (Tab switches view there), Enter or click opens /mcp. Prompt arguments bind positionally, the last one taking the rest of the line; a registry command of the same name wins over an MCP prompt.
Validation: cargo nextest run --workspace (1693 passed), clippy -D warnings and fmt clean for touched crates.

Review fixes (both findings verified real):
1. Resource redaction: attach() labeled read.text() with only the core labeler (session redactor), not the server's env.*/header.* values. Now attach_read() and embedded prompt resources redact with ServerHandle::redact before labeling (text, uri attribute, attachment label). Test attached_resources_are_redacted_with_the_servers_own_credentials (test server gained an unlisted test://env/<NAME> resource); confirmed it fails without the fix (variable name avoids the built-in *TOKEN= pattern, which masked it by accident in a first version of the test).
2. Driver blocking: prompt lookup/start, prompts/get, resources/read and disable/enable/restart now run in McpUi background tasks (aborted with the session in McpUi::stop, so finish_core/quit never wait on a server start). Prompt text returns over an internal channel that Driver::run selects on (Driver::prompt). Disable/enable record the wanted state in Shared::disabled; sync_disabled() applies it and re-checks so quick toggles converge; panel buttons/status show the wanted state immediately and a toast reports completion. Re-applying session disables on a rebuilt session is background too. Test interrupt_and_panel_buttons_work_while_a_slow_mcp_prompt_starts_its_server (60s startup delay: /slow:greet and disable return within 10s, Interrupt cancels the turn, finish_core ends promptly). Existing tests adapted to wait for the background result (mcp_turn helper, completion toasts); no assertion removed.
Docs: docs/mcp.md (background work, redaction with server credentials).
Validation: cargo nextest -p harness -p harness-mcp 234 passed (MCP TUI tests 5x repeated green), clippy -D warnings and fmt clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added MCP to the TUI. /mcp panel lists servers with status (connected, starting, failed + reason, disabled, idle), transport, tool count and auth, with restart / disable-or-enable for the session / logs buttons (clickable, Tab+Enter). MCP prompts are /<server>:<prompt> slash commands and palette entries with their arguments (missing ones asked in the palette); resources attach to the next message from the palette, labeled untrusted via Source::McpResource and redacted with the server's own credentials. Server work (prompt start/get, resource reads, restart/disable/enable) runs in background tasks so Interrupt and Quit are never held up by a slow server. The status line shows a red ⚠ mcp indicator for failed servers that opens /mcp by click or keyboard. New harness-mcp API: prompts module, ServerHandle::{list_prompts,get_prompt,list_resources,read_resource,restart,disable_for_session,enable_for_session,is_disabled_for_session,watch_status,status_generation,connect_count}, Status::{Starting,DisabledForSession}, McpError::DisabledForSession. Verified with harness-mcp tests/prompts_resources.rs, harness tui/tests/mcp.rs (panel, keyboard and mouse, prompts, resources, indicator, credential redaction, interrupt during a slow server start), harness-tui and harness-policy tests; nextest -p harness -p harness-mcp green, clippy and fmt clean.
<!-- SECTION:FINAL_SUMMARY:END -->
