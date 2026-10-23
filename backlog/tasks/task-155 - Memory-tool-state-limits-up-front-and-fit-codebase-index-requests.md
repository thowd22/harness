---
id: TASK-155
title: 'Memory tool: state limits up front and fit codebase-index requests'
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 16:14'
updated_date: '2026-10-23 19:22'
labels:
  - memory
  - agent
dependencies: []
priority: medium
ordinal: 159000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Evidence: a real v0.3.0 session on a large repo (run 01a1691a, sonnet 5.5 via OpenRouter): 126 model calls, about 17 minutes of model time, 15.3M input tokens. The user asked 'index this codebase and store it in memory'. Of about 37 memory calls, 15 writes were rejected with 'memory not saved: the memory is longer than 400 characters' and 1 as task state; the tool description (crates/harness-memory/src/tool.rs) never mentions the 400-character limit, so the model retried and rewrote the same notes 2-3 times.

State the limits in the description and schema (maxLength on text, one bullet per write), make the rejection message say exactly how to split (e.g. suggest writing several bullets), and consider accepting several bullets in one write (text as an array, each validated). Also decide how harness should answer 'index this codebase into memory' given the Devin-style rules (no facts easy to rediscover): e.g. save only non-obvious gotchas and a compact project overview note with a higher limit, and explain this to the user, documented in docs/memory.md.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The memory tool description/schema state the per-bullet limit and one-bullet rule; rejections name the limit and how to split
- [x] #2 A write can carry several bullets (each validated) or the model is told to batch writes in one turn; tests cover both limits and the error text
- [x] #3 There is a documented, tested behaviour for 'index this codebase into memory' requests that does not end in repeated rejected writes
- [x] #4 Defaults CHANGELOG entry if tool descriptions or prompts change; docs/memory.md updated
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
1. guard.rs: limit_for(path) (400 per bullet; 1000 in projects/<key>/overview), length rejection names the actual length, the limit and how to split (several bullets in one write via bullets: [...]); check_format_with(text, max).
2. tool.rs: write accepts text (one bullet) or bullets (array, max 10, each validated); valid bullets saved in one commit, rejected ones reported by index with reason; all rejected -> error. Overview note capped at 10 bullets (enforced in code).
3. Description + schema: maxLength on text and bullets items, one bullet per item, limits stated; guidance for 'index this codebase into memory' (project/overview + non-obvious gotchas only, tell the user the rest is re-read from the repo). Same sentence in extract::PROMPT_SECTION and assets/prompts/memory.md.
4. Tests: guard unit tests (limits, error text), tool integration tests (batch write partial/all-rejected, overview limit and cap, index-codebase flow), description assertions.
5. Update golden snapshots, docs/memory.md, doc-4 if needed, CHANGELOG Defaults entry with fingerprint.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented: guard::limit_for/validate_for (400 per bullet, 1000 in projects/<key>/overview.md), length rejection names length, limit and how to split into `bullets: [...]`. MemoryArgs::Write gains `bullets` (serde default; max 10), each validated; valid ones saved in one commit, rejected listed by number. Overview note capped at 10 bullets (enforced in tool). Description, schema (maxLength, maxItems), extract::PROMPT_SECTION and assets/prompts/memory.md updated. Tests: crates/harness-memory/tests/limits.rs (6 tests) + guard unit test; cargo nextest -p harness-memory 67/67 pass, clippy clean, harness-agent initial_context budget passes. Golden snapshots: applied only the memory description/schema/prompt-section changes to request_coder.snap and prompt_coder.snap (other batch tasks' grep/glob tools, Tools prompt layer and OpenRouter cache_control changes are still pending in the tree and make golden_requests fail independently; integrator reconciles). Defaults CHANGELOG entry added with fingerprint 45850933b6b6, computed with other in-flight default changes present. Docs: docs/memory.md (new 'Index this codebase into memory' section), docs/guide/memory.md, doc-4. DoD#4 N/A: no TUI changes.

Integration (TASK-151..155, 2026-10-23): workspace green on the combined tree with TASK-149/150 also Done: cargo fmt --all --check ok; cargo clippy --workspace --all-targets --locked -D warnings ok; cargo nextest run --workspace --locked --profile ci 1838 passed / 10 skipped (golden_requests and prompt snapshots included); cargo test --workspace --doc ok; cargo deny check ok; scripts/hermetic-guard.sh nextest -p harness -p harness-eval 216 passed, nothing written to HOME/XDG. CHANGELOG Defaults: the provisional TASK-152/153/155 entries are merged into one reconciled entry (also naming TASK-151's cache_ttl and TASK-153's [tools] job limits) with final fingerprint 45850933b6b6; defaults_changelog passes. README gains a Focused tools highlight and the harness-tools crate row. 'Passes in CI' is pending until the orchestrator pushes; nothing is committed.

Open follow-up (not started, needs user approval): the end-of-session memory extractor still applies the default 400-character limit to proposals for project/overview (where the tool allows 1000).

Committed in 58b1512; CI green on Ubuntu and macOS: https://github.com/thowd22/harness/actions/runs/40009831788.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Memory tool now states its limits up front (400 chars per bullet, 1000 in project/overview; schema maxLength/maxItems), accepts up to 10 bullets per write validated individually in one commit, and rejections give the length, limit and how to split. 'Index this codebase into memory' is answered with a compact, code-capped project/overview note plus gotchas, documented in docs/memory.md and taught by the tool description and memory prompt layer. Verified with harness-memory tests (limits.rs, guard unit tests), initial-context budget test, golden snapshot diff review and the defaults changelog fingerprint.
<!-- SECTION:FINAL_SUMMARY:END -->
