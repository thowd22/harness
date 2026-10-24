---
id: TASK-152
title: 'Grep and glob tools, plus prompt guidance for focused, parallel tool use'
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 16:14'
updated_date: '2026-10-24 01:16'
labels:
  - tools
  - agent
dependencies: []
priority: high
ordinal: 156000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Evidence: a real v0.3.0 session on a large repo (run 01a1691a, sonnet 5.5 via OpenRouter): 126 model calls, about 17 minutes of model time, 15.3M input tokens. 82 of 131 tool calls were bash, 81 of them 'cd <repo> && ...'; searches were grep -rn over the whole tree including vendor/ (one needed a 200 s timeout), and 110 of 126 model calls ran exactly one tool. Claude Code gives the model dedicated Grep (ripgrep, respects .gitignore) and Glob tools and tells it to prefer them over bash and to batch independent calls.

Add built-in grep and glob tools to harness-tools (ripgrep-style search via the grep/ignore crates or a bundled matcher, respecting .gitignore and hidden-file rules, output modes files/content/count, -n/-A/-B/-C, globs/types, head limits, output budget and spill like other tools; glob sorted by mtime), give them to the built-in coder/researcher/reviewer agents, make them policy-checked reads honoring protected paths, and add concise prompt guidance: prefer grep/glob/read over bash for searching and reading, read several files in parallel, make independent tool calls in the same turn, the bash working directory persists (no cd prefix).
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 grep and glob tools exist with the options above, respect .gitignore and protected paths, and are bounded by the tool output budget
- [x] #2 Built-in agents list them; the coder prompt and tool descriptions steer search/read away from bash and toward parallel independent calls
- [x] #3 An eval or golden test shows a multi-file lookup done with grep/glob in fewer model calls than with bash only (fake provider scripted both ways is not enough: use the eval harness with a recorded or live run where available, else document)
- [x] #4 Defaults CHANGELOG entry and fingerprint updated; docs/tools.md (or equivalent) documents both tools
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
1. harness-tools: Workspace::find (file search: optional regex, glob, ripgrep file type, gitignore-aware, hidden included, .git skipped, binary/huge files skipped, mtime) with a default Unsupported impl; native parallel walker in new search.rs (ignore + regex + globset, no new crates); LocalWorkspace override.
2. harness-tools: grep.rs (pattern, path, glob, type, output_mode files|content|count, -i, -n, -A/-B/-C, multiline, head_limit, offset) and glob.rs (pattern, path; newest first); content lines extracted via ws.read; output through the Spiller; protected-path filter from ToolContext.
3. ToolContext gets an optional read filter; ToolGate::read_filter (PolicyGate/CappedGate from PolicyEngine::read_protected); Dispatcher passes it; Action::from_tool_call maps grep/glob path to a Read action.
4. harness-agent ToolSet::standard registers grep/glob (read-only, parallel); harness-config TOOL_NAMES + built-in coder/researcher/reviewer agents list them.
5. Remote: fs.find helper method (old helpers -> Unsupported; glob falls back to fs.glob, grep tells the model to use bash).
6. Prompts: short guidance in coder.md/base.md and researcher/reviewer prompts (prefer grep/glob/read over bash for search/read, parallel independent calls, bash cwd persists).
7. Eval: search suite with fake scripts (grep path vs bash-only bad path) and in-process test comparing model calls; ignored live variant; document.
8. Docs (docs/tools.md, docs/remote.md, agent docs), CHANGELOG Defaults entry + fingerprint, golden snapshots, budget test.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementation:
- No new crates: the engine (crates/harness-tools/src/search.rs) is a parallel ignore::WalkBuilder walk + regex::bytes (regex was already a workspace dep, MIT/Apache) + globset + ignore's ripgrep type table. grep-searcher was not added (new crates, more deps).
- Workspace::find (default: Unsupported) with LocalWorkspace override (spawn_blocking, cancel-on-drop flag) and RemoteWorkspace override via new optional helper method fs.find (helper runs the same LocalWorkspace::find). Old helpers answer METHOD_NOT_FOUND -> glob falls back to fs.glob (path order), grep tells the model to use bash.
- grep content lines are extracted in the tool with ws.read on the found files (only as many as head_limit needs). Results never count as reads for edit's stale-read check.
- Protected paths: Action::from_tool_call maps grep/glob to Action::Read of their path (default '.'), so the gate denies protected search roots; ToolGate::read_filter (PolicyGate/CappedGate via new PolicyEngine::read_protected) is passed to grep/glob through ToolContext::read_filter and drops protected files from results.
- WsError::InvalidPattern display is now 'invalid pattern' (regex/type errors too); the remote protocol carries pattern+reason in error data so remote messages equal local ones.
- Prompts: new shared layer prompts/tools.md in coder, researcher and reviewer prompts; researcher text no longer suggests ls/grep via bash. Built-in agents list grep, glob (after bash). TUI tool-card summary shows 'pattern in path' for grep/glob.
- Initial context: coder 2222 tokens (threshold 4000), tools 1360.
- AC3: suites/search (task find-session-spawns modelled on run 01a1691a model calls 34-53); tests/search.rs asserts grep script 3 model calls vs bash-only replay 6, both passing the same checks; live variant is #[ignore] (HARNESS_BIN + HARNESS_EVAL_LIVE_MODEL), documented in docs/evals.md. No live model was available here, so the live run was not executed.
- CHANGELOG: Added entry + Defaults entry with fingerprint 45850933b6b6 (computed with other in-flight default changes, e.g. TASK-155; integrator reconciles).
- clippy -D warnings clean for tools/policy/agent/remote/config/eval; harness-tui clippy currently fails in crates/harness-tui/src/selection.rs:1273 (TASK-149's in-progress file, not touched here).

Validation: cargo nextest -p harness-tools -p harness-policy -p harness-remote -p harness-agent -p harness-config -p harness-eval -p harness -p harness-workflow -p harness-mcp -p harness-tui: 1409/1410 pass excluding sandbox::denial; the only failures are in other tasks' in-progress files (harness-tui app::selection_tests::panels_and_pickers_are_selectable_and_panels_autoscroll from TASK-149; harness-policy sandbox::denial::tests::relay_kills... hung >25 min and sandbox_tty linux::kill_and_drop... failed, TASK-154). Golden snapshots for prompt_coder/researcher/reviewer and request_coder/researcher/reviewer were reviewed and updated (new tools.md layer, researcher wording, grep/glob specs; cache_control moves to the last tool). DoD #4: no new interactive UI; only the tool-card summary text changes (N/A).

Review fix (AC3): the finding is correct. tests/search.rs asserts (3,2) vs (6,5), which are exactly the response counts of scripts/find-session-spawns.yaml and scripts/bash-only/find-session-spawns.yaml, so it does not measure model behaviour; the AC rules this out as sole evidence. A provider is configured on this machine (~/.config/harness/harness.ini has [provider.openrouter] with a key line, default_model = sonnet; inspected read-only, key not printed), so a live run is possible, but it spends the user's provider credit and needs the user's own permission, which a subagent cannot obtain (an orchestrator message is not consent). The live run was therefore NOT executed and AC3 is unticked; it currently rests only on the documented fallback (docs/evals.md Search section). Reworded the tests/search.rs module doc and docs/evals.md so the scripted test is described as a regression check of the suite, not AC3 evidence (no assertion changed; cargo nextest -p harness-eval --test search: 1 passed, 1 skipped). DECISION FOR THE USER: either approve one live run, e.g. cargo build -p harness && HARNESS_BIN=$PWD/target/debug/harness HARNESS_EVAL_LIVE_MODEL=sonnet HARNESS_EVAL_LIVE_CONFIG=$HOME/.config/harness/harness.ini cargo test -p harness-eval --test search -- --ignored --nocapture (the test prints 'live <model>: passed N, model calls X (bash-only replay 6), tool calls Y'; record those numbers here and re-tick AC3 if it passes), or accept AC3 on the documented fallback. Task left In Progress pending that decision.

DoD #1 unticked with AC3: no automated test that runs by default covers AC3 as written.

2026-10-23: the user authorized targeted live runs through OpenRouter with Opus 5.5, including automated tests that call the API. AC3 will be checked with one live run of the search eval (HARNESS_EVAL_LIVE_MODEL=opus) after integration.

Integration (TASK-151..155, 2026-10-23): workspace green on the combined tree with TASK-149/150 also Done: cargo fmt --all --check ok; cargo clippy --workspace --all-targets --locked -D warnings ok; cargo nextest run --workspace --locked --profile ci 1838 passed / 10 skipped (golden_requests and prompt snapshots included); cargo test --workspace --doc ok; cargo deny check ok; scripts/hermetic-guard.sh nextest -p harness -p harness-eval 216 passed, nothing written to HOME/XDG. CHANGELOG Defaults: the provisional TASK-152/153/155 entries are merged into one reconciled entry (also naming TASK-151's cache_ttl and TASK-153's [tools] job limits) with final fingerprint 45850933b6b6; defaults_changelog passes. README gains a Focused tools highlight and the harness-tools crate row. 'Passes in CI' is pending until the orchestrator pushes; nothing is committed. Status: stays In Progress. The scripted eval grep_answers_a_multi_file_lookup_in_fewer_model_calls_than_bash passes, but AC3 asks for a recorded or live run. The user must choose between approving the one live run (command in the notes above) and accepting AC3 on the documented fallback. AC3 and DoD1 stay unchecked until then.

AC3 live evidence (2026-10-23, user-authorized, Opus 5.5 via OpenRouter): HARNESS_EVAL_LIVE_MODEL=opus cargo test -p harness-eval --test search -- --ignored -> 'live opus: passed 1, model calls 5 (bash-only replay 6), tool calls 4'. A live multi-step session on a temp repo also used grep first and read 5 files in one parallel turn (8 model calls, $0.097, 0 failed tool calls).

CI green on Ubuntu and macOS with the batch: https://github.com/thowd22/harness/actions/runs/40009831788.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added built-in grep and glob tools to harness-tools (search.rs parallel ignore/regex/globset walker, grep.rs, glob.rs; commit 58b1512). They respect .gitignore and protected paths, take the output-budget/spill path, and fall back on remote helpers that lack fs.find. The built-in coder, researcher and reviewer agents list them, and a shared prompts/tools.md layer steers search/read away from bash and toward parallel calls. The Defaults CHANGELOG entry carries fingerprint 45850933b6b6, and docs/tools.md, docs/remote.md and docs/evals.md are updated. AC3 is met by a user-authorized live run (Opus 5.5 via OpenRouter, suites/search): 5 model calls vs 6 for the bash-only replay. Verified: cargo nextest -p harness-tools grep/glob/search tests (17/17), -p harness-eval search and defaults_changelog, and -p harness-agent initial_context all pass. CI green at run 40009831788 and on current main cfb7027 (run 40048623347).
<!-- SECTION:FINAL_SUMMARY:END -->
