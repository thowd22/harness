---
id: TASK-25
title: Policy engine
status: Done
assignee:
  - '@policy'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - policy
  - security
  - problem-7
  - problem-8
milestone: m-2
dependencies:
  - TASK-2
  - TASK-20
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 25000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Permissions must be enforced in code with clear precedence (ADR-6). The repo can ask, only the user can grant: project YAML may only tighten policy.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Decisions are allow, ask or deny for every tool call, evaluated as built-in denies, then ini, then agent or workflow YAML (tighten only), then session approvals
- [x] #2 Built-in protected paths include harness.ini, the trust store, data_dir internals and .git/hooks, and are denied for read and write
- [x] #3 User-configured protected paths from policy section apply to read, write, edit and bash path arguments
- [x] #4 Attempts by YAML to loosen policy are rejected at load time with an explanation
- [x] #5 Every decision emits an event with the matching rule
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
1. rules.rs: Decision/Action/MatchedRule/Verdict, PolicyEngine with layers built-in > ini > YAML overlay (tighten-only) > session approvals, mode defaults.
2. Built-in protected paths from DataDir::protected_paths + config file + **/.git/hooks/**; user paths.protected globs; checked for read/write/edit and bash path args (via shell parser).
3. PolicyOverlay deserialisation rejects loosening keys (bash.allow, outward.allow, ...) with an explanation.
4. DecisionSink trait; PolicyEngine::evaluate emits a DecisionRecord with the matched rule for every decision.
5. Tests per AC, docs/policy.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_policy::rules: Decision{Allow,Ask,Deny}, Mode, Action (from_tool_call for read/write/edit/bash), Layer, MatchedRule, Verdict, PolicyConfig (from_config/from_ini over harness_config Config + DataDir::protected_paths), PolicyOverlay (tighten-only, serde try_from; From<&harness_config::agents::PolicyFragment>), PolicyEngine (decide/check, with_sink, with_grants, with_overlay), DecisionSink/DecisionRecord/RecordingDecisions. Precedence: built-in protected paths (harness.ini, data dir incl. trust store, .git/hooks, writes to .harness/; tree writers like rm -rf on an ancestor) > ini deny > YAML deny > outward guard > ini/YAML ask > ini allow (static commands only) > mode default; session grants turn ask into allow only. Paths: ~, $HOME, $PWD expansion, wildcard expansion, symlink + .. resolution. Tests: rules::tests (19), overlay/paths/pattern unit tests. REMAINING for AC#5: every decision is emitted as a DecisionRecord (with matched rule) to the engine's DecisionSink, but harness-events has no policy-decision EventKind, so decisions are not yet written to the run event log. Needs harness-events to add e.g. EventKind::PolicyDecision{call_id, tool, decision, layer, origin, rule, reason} and harness-agent to implement DecisionSink over RunRecorder; then add an integration test and check AC#5.

Integration: harness-events EventKind::PolicyDecision; PolicyGate::with_recorder/with_decisions writes every decision with its matched rule to the run log (harness-agent dispatch tests, headless.rs E2E shows policy_decision events).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: harness-events EventKind::PolicyDecision; PolicyGate::with_recorder/with_decisions writes every decision with its matched rule to the run log (harness-agent dispatch tests, headless.rs E2E shows policy_decision events). Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
