---
id: TASK-38
title: Inline approval prompt with preview
status: Done
assignee:
  - '@tui-views'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:19'
labels:
  - tui
  - problem-7
milestone: m-3
dependencies:
  - TASK-117
  - TASK-27
  - TASK-32
  - TASK-39
priority: high
type: feature
ordinal: 38000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Approvals must be fast to answer and give enough information to decide.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Approval appears inline above input with y once, a session pattern, n deny, and Esc
- [x] #2 Edits and writes show a diff preview, bash shows the parsed command and matching rule
- [x] #3 Batched approvals can be answered together or individually
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
1. approval::ApprovalPanel over harness_policy ApprovalPrompt: per-request answers, focus, y/s/a/n, Y/N for batches, Esc denies the rest. 2. Cards with parsed command + matched rule, diff previews via cards::parse_unified/diff_lines. 3. ApprovalQueue replies on PromptEnvelope.reply. 4. IniConfirm for 'always' rules. 5. ChatView lays the panel out above the input and tints the tool card.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/approval.rs (+ ChatView::push_approval). Bash requests show verdict.command (the deciding simple command) and layer/origin/rule/reason; write/edit previews render as diffs. Buttons are registered regions (approval.N.once/session/always/deny, approval.all.once/deny) usable by click and Tab/Enter. Tests: single_request_keys_once_session_always_deny, bash_shows_parsed_command_and_rule_edits_show_diff, batch_answers_individually_or_together, buttons_are_clickable_and_keyboard_focusable, queue_replies_on_the_envelope (real ChannelUi), ini_confirmation_writes_only_on_yes, and ChatView approval_appears_inline_above_input_and_answers_by_key_or_click. Integration: the binary must forward ChannelUi's receiver into ChatView::push_approval, and show IniConfirm when an ApprovalResolution carries ini_rule.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Inline approval cards above the input with once/session/always/deny, diff previews for writes/edits, parsed command and matching rule for bash, batch answers, replies through ChannelUi envelopes, and a separate ini rule confirmation. Verified by harness-tui tests; documented in docs/tui-views.md.
<!-- SECTION:FINAL_SUMMARY:END -->
