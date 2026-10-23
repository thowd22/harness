---
id: TASK-156
title: >-
  Provider plugins: custom model endpoints and auth without changing harness
  source
status: To Do
assignee: []
created_date: '2026-10-23 18:51'
labels:
  - providers
  - auth
  - plugins
dependencies: []
priority: medium
ordinal: 160000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Goal: a company can point harness at its own model endpoint (example: an internal model router on Azure behind company SSO) and plug in its own auth, with no change to harness source. Core auth for Claude, OpenAI (API key + Sign in with ChatGPT) and OpenRouter stays built in.

Findings (2026-10-23):
- Providers are hard-wired: crates/harness/src/wiring.rs provider_sync matches anthropic | openai | openrouter and errors 'unknown provider' otherwise; harness login knows only those three.
- Credentials are static: api_key / api_key_cmd / api_key_env resolve once to a key placed in a default header (Bearer or x-api-key). Short-lived tokens (Azure AD / Entra ~1h, company SSO) would expire mid-session.
- There is already a per-request auth path: OpenAiProvider's Auth::ChatGpt gets a fresh token per call from ChatGptAuth; MCP already supports header.<Name> with _cmd/_env and OAuth. These are the patterns to generalize.
- HttpClient (harness-providers/src/http.rs) takes default_headers and a base_url; wire formats exist for Anthropic Messages, OpenAI Responses and OpenAI-compatible Chat Completions (the OpenRouter request/stream modules).

Design (recommended): two layers, both configured only in the user's harness.ini (a protected path; project YAML can never add providers or auth commands).
1. Declarative custom providers: [provider.<id>] with wire = anthropic | openai-responses | openai-chat, base_url, path/query.* (e.g. Azure api-version), header.* (static, _cmd, _env), and model facts in [models]/[pricing] (context window, tools, prices). Covers Azure OpenAI, Azure AI Foundry, LiteLLM, vLLM and most company gateways.
2. Exec auth plugins (credential-helper protocol, like kubectl exec credential plugins, AWS credential_process, git credential helpers): auth = exec, auth_command = <program and args>. harness runs it with a JSON request on stdin ({version, provider, base_url, reason: initial | expired | unauthorized | login, interactive}) and reads JSON ({headers, query?, expires_at?}) from stdout. Results are cached until shortly before expires_at, refreshed on expiry and once on 401/403, and treated as secrets (redacted everywhere). Plugins are any executable (shell, Python, Go, Rust), so no ABI or recompile; WASM or dynamic-library plugins are explicitly out of scope (ABI churn, unsafe loading, sandboxing cost).

Effort: moderate. The provider trait, wire formats and HTTP stack are reusable; the work is a generic provider builder, a per-request auth source abstraction, the exec protocol, and docs/test kit. Split into subtasks.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A custom provider and an exec auth plugin, configured only in harness.ini, run agent turns end to end against a local mock router (no harness source change), with expiring tokens refreshed mid-session
- [ ] #2 The built-in anthropic, openai (API key and ChatGPT) and openrouter paths are unchanged (golden snapshots and existing tests pass)
- [ ] #3 docs/providers-plugins.md explains the config, the plugin protocol (versioned), security model and an Azure example
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
