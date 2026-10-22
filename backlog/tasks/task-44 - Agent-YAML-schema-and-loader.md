---
id: TASK-44
title: Agent YAML schema and loader
status: Done
assignee:
  - '@yaml-config'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:32'
labels:
  - config
  - agent
  - problem-7
milestone: m-4
dependencies:
  - TASK-43
  - TASK-9
  - TASK-25
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
priority: high
type: feature
ordinal: 44000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Agents are defined in YAML with explicit model selection, tools, limits, scope and placement, giving users control over delegation.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Supports every agent field in the YAML spec
- [x] #2 Model alias, prompt, subagent and remote references are validated at load
- [x] #3 Policy fields can only tighten and loosening is rejected
- [x] #4 Agent tools list controls exactly which tools appear in requests
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
1. agents::Agent with every doc-3 field (model, effort, prompt|prompt_text, tools, subagents, memory, max_turns, budget, scope, policy, verify, runs_on, output_schema).
2. validate against ValidationContext (model aliases/providers from ini, prompt names, agent names, remotes/labels).
3. Local PolicyFragment type; tighten-only (reject bash.allow / unknown loosening keys, project agents may not widen shadowed agent tools/budget/memory/max_turns or exceed ini budgets).
4. effective_tools / select_tools controls request tool list exactly.
5. Tests + docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_config::agents: Agent with all doc-3 fields (model, effort, prompt|prompt_text, tools, subagents, memory, max_turns, budget{usd,tokens}, scope, policy, verify, runs_on local|remote|{labels,min_mem_gb}, output_schema). File-local checks in Artifact::load; cross-file validate()/Catalog::validate_agents against ValidationContext::from_config (ModelRefs aliases+providers, remotes+labels, [budget] run_usd cap) and the Catalog (prompts, subagents). Local PolicyFragment (deny/ask/protected/outward only); bash.allow and unknown keys rejected; tighten_violations() for project agents shadowing lower-scope ones. Agent::request_tools/select_tools define exactly which tools go in requests (tools + spawn_agent if subagents + memory if memory != off). cargo test -p harness-config: 88 unit + 6 integration tests pass; cargo clippy -p harness-config --all-targets -D warnings clean; cargo fmt check clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Agent YAML loader with every spec field, load-time validation of model/prompt/subagent/remote refs, tighten-only policy (local PolicyFragment) and exact request tool selection. Verified by src/agents/tests.rs; documented in docs/yaml-config.md and doc-3.
<!-- SECTION:FINAL_SUMMARY:END -->
