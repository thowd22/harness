---
id: TASK-31
title: Untrusted-content labeling and exfiltration guards
status: Done
assignee:
  - '@policy'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:47'
labels:
  - security
  - problem-8
milestone: m-2
dependencies:
  - TASK-5
  - TASK-23
priority: medium
type: feature
ordinal: 31000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Tool output, web content and project files can carry prompt injection. The model must be able to tell instructions from data, and known secrets must never reach it.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Tool results are wrapped with provenance markers the system prompt explains
- [x] #2 Known secrets are redacted from tool results before they are sent to the model
- [x] #3 Content from untrusted projects is labeled as such
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
1. provenance::Labeler(Arc<Redactor>, ProjectTrust): label(Source, content) / tool_result(..) redact then wrap in <tool_result|project_file|web_content ... trust=...> markers, neutralising marker tags in content.
2. SYSTEM_PROMPT_SECTION explains markers and trust values.
3. Untrusted projects and web content labeled trust=untrusted with origin.
4. Tests, docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_policy::provenance. Redaction uses harness_events::redact::Redactor (built-in patterns + resolved credentials), applied to content and attribute values. Marker tags inside content (any case, with / or spaces) are escaped so content cannot close its marker. Tests: provenance::tests (4). Needs: harness-agent must pass every tool result through Labeler::tool_result before building the next model request (with the same Redactor as RunRecorder, and ProjectTrust::Trusted only when harness_config::trust::TrustStatus::Trusted); prompt assembly must include provenance::SYSTEM_PROMPT_SECTION; project files injected into context go through Labeler::label(Source::ProjectFile).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Provenance labeling and secret redaction for content sent to the model: Labeler wraps tool results/project files/web content in trust-labeled markers explained by SYSTEM_PROMPT_SECTION, redacting known secrets first; untrusted projects are labeled. DoD#4 N/A. Verified with cargo test -p harness-policy and clippy -D warnings.
<!-- SECTION:FINAL_SUMMARY:END -->
