---
id: TASK-104
title: runs_on in agent and workflow YAML
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - remote
  - workflow
  - config
milestone: m-10
dependencies:
  - TASK-102
  - TASK-44
  - TASK-53
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
priority: high
type: feature
ordinal: 104000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The user wants remote placement configurable directly in agent and workflow YAML.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 runs_on accepts local, a host name, or a selector with labels and resource minimums plus fallback
- [x] #2 Scheduler picks hosts using labels, probe results and max_jobs
- [x] #3 Unknown hosts in YAML are load errors
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
1. Workflow schema runs_on: local | <host> | {labels, min_mem_gb, min_cpus, min_gpus, fallback: local|<host>}; unknown hosts and unmatchable selectors without fallback are load errors (CheckContext.remotes from harness.ini).
2. harness_workflow::placement::HostPool (from HostRegistry + ProbeCache): labels, probe resource minimums, reachability, max_jobs leases, least-busy choice, waiting, fallback.
3. Executor places runs_on steps, records the host on node_started and passes StepRequest.host.
4. from_agent() converts agent YAML runs_on.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done for workflows: schema.rs runs_on parsing and validation; placement.rs (6 tests: labels/resources, least busy, max_jobs waiting, named hosts, fallbacks, unreachable, from_registry over ini + ProbeCache); executor integration runs_on_steps_are_placed_on_matching_hosts (host on the node, leases released). Agent YAML unknown hosts are already load errors in harness_config::agents::validate. Remaining: (1) AC#1 for agent YAML: harness_config::agents::RunsOn::Labels lacks fallback, min_cpus and min_gpus; the yaml-config owner should add them, after which placement::from_agent maps them. (2) The binary or a remote-aware StepRunner must actually execute placed steps remotely (sync::RemoteRun push/pull/apply around the step, RemoteWorkspace/RemoteExec tools); today StepRequest.host is passed but the standard runners run locally. Validation: cargo test -p harness-workflow passes; clippy -D warnings clean.

Integration: agent RunsOn::Labels gains min_cpus, min_gpus and fallback with validation and schema; placement::from_agent maps every field (placement tests agent_selectors_map_every_field). Note: placed steps run locally in this build (documented in docs/workflows.md); remote execution of steps is tracked with TASK-102.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: agent RunsOn::Labels gains min_cpus, min_gpus and fallback with validation and schema; placement::from_agent maps every field (placement tests agent_selectors_map_every_field). Note: placed steps run locally in this build (documented in docs/workflows.md); remote execution of steps is tracked with TASK-102. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
