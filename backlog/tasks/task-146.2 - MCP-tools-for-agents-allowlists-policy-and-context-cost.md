---
id: TASK-146.2
title: 'MCP tools for agents: allowlists, policy and context cost'
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 06:01'
updated_date: '2026-10-23 08:36'
labels:
  - tools
  - mcp
  - policy
  - problem-6
  - problem-7
  - problem-8
milestone: m-2
dependencies:
  - TASK-146.1
  - TASK-25
  - TASK-31
  - TASK-44
references:
  - 'https://modelcontextprotocol.io/specification'
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
parent_task_id: TASK-146
priority: high
type: feature
ordinal: 148000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
MCP tools must behave like built-in tools: visible, permission-checked and cheap in tokens. Large servers expose dozens of tools, so sending every schema on every request would blow the initial-context budget.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Agent and workflow YAML select MCP tools explicitly, e.g. tools: [read, edit, mcp:github/create_issue, mcp:db/*]; nothing from MCP is exposed unless selected, and project YAML can only reference servers defined in harness.ini
- [x] #2 Every MCP tool call goes through the policy engine with allow, ask or deny rules per server and tool (for example mcp.github.ask = create_*), outward-action defaults for tools that write externally, and session-scoped approvals
- [x] #3 MCP tool results are labeled as untrusted content (provenance markers), redacted, size-capped with spill files, and recorded as events with server and tool names
- [x] #4 Context cost is bounded: when selected MCP tools exceed a configurable schema token budget, they are exposed through a single search-and-load tool instead of full schemas; the initial-context CI budget still passes with a large test server configured
- [x] #5 Tool cards, the map and node detail show MCP calls with server name, tool name, duration and result, clickable and keyboard-operable
- [x] #6 Server tool-list changes (notifications) refresh the available tools without restarting the session, with the stable prompt prefix only changing at session boundaries
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
1. harness-config: mcp:<server>/<tool> selectors in agent tools: (Agent.mcp_tools, load-time server check via ValidationContext.mcp_servers, tighten check, JSON schema); [policy] mcp.<server>.allow|ask|deny|trust_annotations; [context] mcp_schema_budget (default 2000, hash-neutral).
2. harness-events: mcp_call and mcp_tools_changed events.
3. harness-policy: mcp module (action names mcp:<server>/<tool>, McpRules, ReadOnlyTools), engine.with_mcp (deny>ask>allow>trusted readOnlyHint>outward ask in every mode), tool session grants, broker suggested pattern + always -> mcp.<server>.allow; provenance labels MCP results untrusted.
4. harness-agent: AgentTool::mcp_target (gate decides mcp:<server>/<tool>, loop records mcp_call), ExtraTools hook on SubagentRuntime (added after built-in selection; ChildRequest.mcp_tools for workflow steps).
5. harness-mcp: notification broadcast on stdio/HTTP, subscriptions/listen (modern) and GET stream (legacy), ServerHandle tool generation + redact; tools::McpToolbox (direct mcp__server__tool tools within budget, else mcp_tools search-and-call; frozen per agent for a stable prefix; refresh on list_changed with events; results redacted, spilled, rendered); test server --agent-tools/--bulk-tools and list_changed support.
6. harness-workflow: agent step tools: [mcp:...] with server check.
7. harness binary: crate::mcp::wire for Core and workflows (rules, extra tools, refresh per turn, shutdown, kill_all at exit, --allow mcp:...).
8. TUI: cards/map live/node detail show mcp <server> › <tool> with duration/result; change notice.
9. Tests for every AC (unit, integration over stdio+HTTP both eras, binary e2e, CI initial-context with a 150-tool server); docs (mcp.md, policy, events, context, workflows, yaml guide, tui-views, doc-1/2/3, CHANGELOG).

Review fix (AC#3): give SubagentRuntime a Labeler (with_labeler), set from core.rs and workflow.rs with the run redactor and project trust, applied to every child AgentLoop; tests for subagent and workflow agent step paths.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Decisions:
- Model-facing names: mcp__<server>__<tool> (sanitized to [A-Za-z0-9_-], cut to 64 chars with a hash). The policy engine sees Action::Tool named mcp:<server>/<tool> (via AgentTool::mcp_target), so no Action enum change was needed.
- Policy order for MCP: [policy] mcp.<server>.deny > ask > allow > readOnlyHint (only with mcp.<server>.trust_annotations = true, MCP annotations are untrusted by default) > outward default (ask in every mode, incl. auto). Session grants are mcp:<server>/<tool> wildcard patterns; 'always' writes mcp.<server>.allow = <tool> after confirmation (IniRuleProposal detects the mcp: prefix). harness run --allow 'mcp:t/*' grants MCP tools. [policy] mcp.*.x (all servers) is not possible: '*' is not a valid ini key character.
- Context cost: selected tool specs are estimated with harness_agent::context::estimate_tools; over [context] mcp_schema_budget (default 2000) one mcp_tools tool (~143 tokens) with search/call/arguments replaces them. Offers are frozen per agent+selectors for the session (stable prefix); list changes update calls/searches immediately and emit mcp_tools_changed; removed tools fail with 'no longer offers'.
- Notifications: transports broadcast server notifications; stdio legacy servers notify unasked, modern ones get subscriptions/listen (long-lived request, pending entry kept); HTTP modern holds a subscriptions/listen POST stream, HTTP legacy the GET stream, re-opened with backoff (404/405 = unsupported). ServerHandle bumps a tool generation on list_changed and on restart; the toolbox re-reads on change (background watcher per server) and at each Core turn.
- Results: redacted with the server's redactor (env/header/OAuth + known secrets), spilled with the run's blob store (16000 bytes), labeled trust=untrusted origin=mcp server by the Labeler for mcp__*/mcp_tools; isError -> tool error.
- Workflow agent steps accept tools: [mcp:...] only (CheckContext.mcp_servers checks servers); ChildRequest.mcp_tools carries them.
- Test server: new tools are behind --agent-tools (existing tool-count tests unchanged); --bulk-tools N for large servers; declares tools.listChanged.
Known limits: steps placed on remote hosts use host gates built without [policy] mcp rules, so their MCP calls always ask (stricter, not looser). Direct (non-gateway) tools added mid-session appear only in the next session by design (AC#6 prefix rule).
Note: ran cargo fmt --all once, which may have reformatted files other engineers had left unformatted (whitespace only).
Tests: harness-mcp tests/agent_tools.rs (11: selection, trusted annotations, policy+labeling+events through AgentLoop, session approvals, redaction/spill/errors/structured, gateway search/call/validation, budget, list changes over stdio modern+legacy and HTTP modern+legacy), tests/initial_context.rs (CI budget with a 150-tool server: coder 1493, coder+mcp:big/* 1636 of 4000), tools.rs unit tests; harness-config agents/mcp_tools tests; harness-policy rules/broker/provenance/mcp tests; harness-agent dispatch test; harness-workflow schema test; harness-tui cards/chat view/map/detail tests (click + keyboard); harness binary tests/mcp_tools.rs (e2e harness run --json with the stdio test server).
Validation: cargo nextest run --workspace 1677 passed; harness-mcp 82 passed (3x nextest, 3x cargo test); clippy --workspace --all-targets -D warnings clean; fmt clean; scripts/hermetic-guard.sh nextest -p harness-mcp and harness mcp_tools: ok; no leftover test-server processes.

Review fix: child AgentLoops (subagents, workflow agent steps) had no Labeler, so MCP results reached the model unlabeled. Confirmed: only core.rs set with_labeler. Fixing by giving SubagentRuntime a Labeler.

Review finding confirmed: child AgentLoops in harness-agent/src/subagents.rs had no Labeler, so MCP results in subagents and workflow agent steps (tools: [mcp:...]) were only redacted, not wrapped as trust="untrusted" origin="mcp server". Fix: SubagentRuntime::with_labeler (applied in run_child); core.rs builds one Labeler (redactor + project trust) shared by the top loop and the runtime; workflow.rs builds the same from the trust store (workflow::labeler). Side effect, intended: child built-in tool results are now provenance-labeled like the top agent's. New tests: harness-agent subagents::tests::children_label_mcp_results_untrusted_with_the_runtime_labeler; harness tests/mcp_tools.rs workflow_agent_steps_label_mcp_results_untrusted (verified failing without the workflow.rs wiring). docs/mcp.md notes labeling covers subagents and workflow steps. Validation: cargo nextest -p harness-agent -p harness-workflow -p harness (334 passed), -p harness-eval -p harness-mcp (142 passed, 2 pre-existing skips), clippy -D warnings clean for harness-agent and harness.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
MCP tools now reach agents only when selected and behave like built-in tools. Agent YAML selects them in tools: (mcp:github/create_issue, mcp:db/*) and workflow agent steps can add some with tools:; servers must be [mcp.<name>] in harness.ini (load-time check). Calls are decided by the policy engine as mcp:<server>/<tool> with new [policy] mcp.<server>.allow/ask/deny/trust_annotations, an outward-action default that asks in every mode, session approvals, always-rules for mcp.<server>.allow and --allow mcp:... in headless runs. Results are labeled untrusted (origin=mcp server), redacted with the server's credentials, size-capped with spill pointers, and recorded with new mcp_call events (plus policy_decision and tool_result). Over the new [context] mcp_schema_budget (default 2000 tokens) one mcp_tools search-and-call tool replaces the schemas; the CI initial-context check passes with a 150-tool server (1636/4000). Tool-list change notifications (stdio, HTTP GET stream, modern subscriptions/listen) refresh tools mid-session with mcp_tools_changed events while each agent's offered definitions stay frozen for the session. TUI tool cards, map live pane and node detail show mcp <server> › <tool> with duration and result (click and keyboard tested). Verified with new tests in harness-mcp (agent_tools, initial_context), config, policy, agent, workflow, TUI and a binary e2e test; full workspace nextest (1677 passed), clippy -D warnings and fmt clean, hermetic guard ok. Docs: docs/mcp.md (tools, permissions, context cost, results, list changes, troubleshooting), policy.md, events.md, context.md, workflows.md, guide/yaml.md, tui-views.md, doc-1/2/3, CHANGELOG (Added + Defaults).

Review fix: MCP results in subagents and workflow agent steps are now labeled untrusted too (SubagentRuntime::with_labeler wired from Core and workflow build); covered by a subagent unit test and a wf run end-to-end test.
<!-- SECTION:FINAL_SUMMARY:END -->
