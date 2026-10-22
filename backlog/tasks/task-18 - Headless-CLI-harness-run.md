---
id: TASK-18
title: 'Headless CLI: harness run'
status: Done
assignee:
  - '@agent-loop'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:15'
labels:
  - cli
  - core
milestone: m-1
dependencies:
  - TASK-16
priority: high
type: feature
ordinal: 18000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Evals, scripting and remote helpers need the engine without the TUI. Headless mode also proves the core is properly decoupled.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 harness run with a prompt executes the default agent in the current directory
- [x] #2 --json streams redacted events as JSON lines
- [x] #3 Exit codes distinguish success, verification failure, budget stop, cancel and error
- [x] #4 Approvals in headless mode follow a flag: deny, or allow-listed only
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
1. crates/harness: cli.rs (hand-rolled RunArgs parser, exit codes), wiring.rs (credentials -> Redactor, Registry with [models]/[pricing], provider factory, default coder agent, system prompt + provenance section), headless.rs (run + sessions).
2. --json prints recorder.subscribe() events (already redacted) as JSON lines.
3. Exit codes 0/1/2/3/4/5/130; ShellVerifier for [verify] command.
4. HeadlessUi denies every approval prompt; allowlist mode pre-grants --allow patterns into SessionGrants.
5. Binary tests with --fake-script.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
main.rs also handles __sandbox-exec (harness_policy launcher) first, 'config check [PATH]' (harness_config::ini::run_check), 'schema [KIND]' (yaml::run_schema), 'sessions'. Startup loads harness.ini via ini::load, resolves all provider credentials, opens DataDir::from_config(..).open() and starts logging with LogConfig::with_redactor(Arc<Redactor>) (closes TASK-6 AC#3 / TASK-5 AC#2 wiring; test log_file_is_redacted_with_resolved_credentials and json_streams_redacted_events). Bash runs through SandboxedExec when [sandbox] enabled. OpenRouter agents call check_agent_model at load (TASK-13 AC#5 wiring, no network test). Project trust for labels: Trusted only when TrustStore::status == Trusted. Tests: crates/harness/tests/headless.rs (10 binary tests: default agent run, --json redaction + event order, exit codes 5/4/1/2, verify 3/0, SIGINT 130, approvals deny, allowlist, resume, config check/schema/version, log redaction) + cli unit tests. cargo clippy -p harness --no-deps --all-targets -D warnings clean (without --no-deps, clippy currently fails inside harness-tui, owned by another engineer). Docs: docs/headless.md.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
harness run: default agent in cwd, --json redacted event stream, distinct exit codes (0/1/2/3/4/5/130), headless approvals deny|allowlist; also sessions, config check, schema and the sandbox launcher entry. Verified by 10 end-to-end binary tests with the fake provider.
<!-- SECTION:FINAL_SUMMARY:END -->
