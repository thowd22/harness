---
id: TASK-26
title: Shell command parsing for policy
status: Done
assignee:
  - '@policy'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:46'
labels:
  - policy
  - security
  - spike
milestone: m-2
dependencies:
  - TASK-25
priority: high
type: spike
ordinal: 26000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Pattern-matching raw command strings is easy to bypass with pipes, subshells or chained commands. Policy must evaluate each simple command.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Spike compares candidate bash parser crates and records the choice
- [x] #2 Compound commands, pipelines, subshells, env prefixes and redirects are split into simple commands, each checked
- [x] #3 Unparseable commands fall back to ask
- [x] #4 Tests cover known bypass shapes
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
1. Spike: compare brush-parser, conch-parser, yash-syntax, tree-sitter-bash, shlex; record choice in docs/policy.md and notes.
2. shell.rs: conservative hand-written parser producing flattened SimpleCommands (lists, pipelines, subshells, groups, $(), backticks, process substitution, env prefixes, redirects, heredocs), wrapper unwrapping (env, sudo, command, bash -c, eval, xargs...).
3. Anything not understood -> ParseError -> policy Ask.
4. Bypass-shape tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Spike: compared brush-parser 0.4 (full bash peg grammar, heavier, second parse for $( ) words), conch-parser (unmaintained, no bash-isms), yash-syntax (async interpreter-oriented), tree-sitter-bash (C dep, error-tolerant = wrong default for security), shlex (tokeniser only). Chose a hand-written conservative parser (crates/harness-policy/src/shell/{parser,wrappers}.rs) that fails closed; comparison recorded in docs/policy.md. Supports lists/pipelines, subshells, groups, if/while/for bodies, [[ ]], $( ), backticks, <( ), quoting incl. $'..' escapes, brace expansion, env prefixes, redirects, heredocs; unwraps env/sudo/doas/command/exec/nohup/time/nice/ionice/timeout/stdbuf/setsid/unbuffer/xargs/watch/sh -c/eval/find -exec. Unsupported syntax (functions, case, (( )), dynamic program names, bash -c "$X", unterminated quotes, substitution in unquoted heredoc, nesting > 32) -> ParseError -> PolicyEngine returns Ask (deny rules still matched against raw text). Tests: shell::tests (13) + rules::tests each_simple_command_is_checked / unparseable_commands_ask (26 bypass shapes). cargo test/clippy -p harness-policy clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Hand-written fail-closed bash parser in harness_policy::shell (parse -> Script of SimpleCommands, wrapper unwrapping, ParseError on anything unknown); PolicyEngine checks every simple command and asks on parse errors. Spike recorded in docs/policy.md. DoD#4 N/A (no UI). Verified by cargo test -p harness-policy (shell + rules bypass tests) and clippy -D warnings.
<!-- SECTION:FINAL_SUMMARY:END -->
