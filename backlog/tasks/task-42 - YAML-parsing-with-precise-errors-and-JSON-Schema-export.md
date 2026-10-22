---
id: TASK-42
title: YAML parsing with precise errors and JSON Schema export
status: Done
assignee:
  - '@yaml-config'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:19'
labels:
  - config
  - spike
milestone: m-4
dependencies:
  - TASK-1
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
priority: high
type: spike
ordinal: 42000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
YAML errors must point at file, line and column, and editors should offer completion. The common serde_yaml crate is unmaintained, so the parser choice needs a spike.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Spike records the chosen maintained YAML crate and why
- [x] #2 Deserialization errors include file, line and column
- [x] #3 harness schema prints JSON Schema for prompts, agents and workflows
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
1. Spike: compare serde_yaml (deprecated), serde_yml, serde_yaml_ng, serde_norway; pick serde_norway (already in workspace).
2. harness_config::yaml: YamlError{file,line,column,message}, from_str/from_file, key locator, strict (deny_unknown_fields) helpers.
3. harness_config::yaml::schema: hand-written JSON Schema (draft 2020-12) for prompt, agent and workflow; run_schema(kind, out, err) for the harness schema subcommand.
4. Tests for error locations and schema output; docs/yaml-config.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Spike result: serde_norway chosen (maintained serde_yaml 0.9 fork on unsafe-libyaml-norway). serde_yaml deprecated, serde_yml soundness/maintenance concerns, serde_yaml_ng pulls unmaintained unsafe-libyaml (RUSTSEC-2024-0449), saphyr serde layer not stable. Recorded in rustdoc of harness_config::yaml. serde_norway gives 1-based line/col and a field path even for deny_unknown_fields; YAML 1.2 has no _ separators so harness_config::yaml::de::lenient_* accept 2_000_000. Externally tagged enums require !tags in serde_norway, so YAML shapes use structs/untagged instead.
Implemented harness_config::yaml (YamlError, parse, read, locate_key), yaml::de, yaml::schema (SchemaKind, schema, run_schema -> exit code 0/2). Schemas are hand-written draft 2020-12 (schemars not in workspace); tests check every doc-3 field is present.
Remaining: AC#3 needs the binary owner to add `harness schema [kind]` in crates/harness/src/main.rs calling harness_config::yaml::run_schema(kind, &mut stdout, &mut stderr) and exiting with its code, plus a binary-level test. The library function is tested. cargo test -p harness-config: 88 unit + 6 integration tests pass; cargo clippy -p harness-config --all-targets -D warnings clean; cargo fmt check clean.

Integration: `harness schema [prompt|agent|workflow]` is wired in crates/harness/src/main.rs; crates/harness/tests/headless.rs::config_check_and_schema_subcommands checks JSON output (exit 0) and an unknown kind (exit 2).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
serde_norway chosen; located YAML errors; `harness schema` prints JSON Schemas. Verified by harness-config tests and the binary test config_check_and_schema_subcommands.
<!-- SECTION:FINAL_SUMMARY:END -->
