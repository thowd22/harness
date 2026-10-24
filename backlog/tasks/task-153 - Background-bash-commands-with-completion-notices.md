---
id: TASK-153
title: Background bash commands with completion notices
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
ordinal: 157000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Evidence: a real v0.3.0 session on a large repo (run 01a1691a, sonnet 5.5 via OpenRouter): 126 model calls, about 17 minutes of model time, 15.3M input tokens. The model polled long commands with 'sleep 280/560/570' because background jobs (nohup cmd &) are killed when the bash call returns and calls are capped at bash_max_timeout (600 s). Claude Code has run_in_background with a notification when the command exits.

Add run_in_background to the bash tool: the command keeps running in its own process group after the call returns, output goes to a spill file the model can read, and when it exits harness appends a short notice (command, exit code, duration, output tail/path) to the agent's next request (like the todo nudge: unsent tail only). Add a way to check/stop background jobs (e.g. a bash_jobs tool or actions on bash). Jobs are killed on session end/cancel (killpg, never negative kill), capped in number, shown in the TUI (activity line / tool card with live status; clickable and keyboard-operable), and work for remote hosts where the helper supports it. Coordinate with TASK-150 (liveness of running tools) which touches the same bash/exec code.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 bash with run_in_background returns immediately with a job id and output path; the job survives the call and is killed on cancel/session end
- [x] #2 When a job exits, the agent's next request carries a notice with exit code, duration and output tail; the model can list, read and stop jobs
- [x] #3 The TUI shows running background jobs with elapsed time and lets the user open or stop them by mouse and keyboard
- [x] #4 Limits (max jobs, output budget) are enforced and documented; tests cover exit notice, cancel cleanup, no orphans, and remote fallback
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
1. harness-tools jobs module: JobManager (per BashTool/session) runs a background ExecRequest in its own process group via any Exec (local, sandboxed, remote helper, shell fallback), drives output into a per-session temp log file (capped by [tools] bash_job_output_bytes) plus an in-memory tail, enforces [tools] bash_max_jobs running jobs, supports list/read-new-output/stop (kill = whole group via the Exec contract, never negative kill), take_finished() for notices, shutdown() kills all and waits; Drop cancels. Emits ToolEvent::Job updates (started/finished) through the starting call's sink.
2. bash tool: run_in_background arg (schema + description); returns job id, output path and how to read/stop; no timeout unless given (not capped by bash_max_timeout); cwd changes in the job do not persist. New bash_jobs tool (action list|output|stop, id) in harness-tools; adapter in harness-agent dispatch, registered in ToolSet::standard and auto-offered right after bash by ToolSet::select; ToolSet::jobs() accessor.
3. Events: additive EventKind::BashJob {call_id, job_id, command, state, exit_code?, duration_ms?, output?}; RecorderToolEvents maps ToolEvent::Job; redaction of command/output path.
4. harness-agent JobNotices RequestHook (TodoNudge pattern): before each request, finished-but-unreported jobs become one short notice (command, exit status, duration, redacted output tail, log path) appended to the unsent tail user message; registered in core.rs; Core::finish shuts jobs down (session end, /new, /resume, Ctrl-C quit path).
5. TUI: status-line indicator '⚙ N jobs' folded from bash_job events, clickable and focusable (opens /jobs); /jobs builtin shows a panel with each job (state, elapsed, command, log) and buttons 'output jN', 'stop jN', 'refresh' (panel buttons are mouse + keyboard). Driver keeps the JobManager handle in Handles so it works during a turn.
6. Tests: jobs unit tests (exit notice data, read offsets, output cap, max jobs, stop kills group, shutdown leaves no orphans, drop cancels), bash run_in_background tests, hook test (notice lands on unsent tail only, once), dispatch event mapping, remote test (helper RemoteExec and ShellExec fallback), TUI status indicator click/keyboard test, driver /jobs panel test.
7. Docs: docs/tools.md (bash run_in_background, bash_jobs, limits), docs/tui*.md, docs/events.md, doc-2 [tools] keys; CHANGELOG (feature + Defaults fingerprint); golden snapshots reviewed.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented as planned. Design: harness-tools::jobs::JobManager (one per session, shared by every agent's BashTool; see the review-fix note below) drives each job's ExecHandle in a tokio task, so every Exec works unchanged (local, sandbox relay, remote helper, shell fallback); kill goes through the ExecHandle contract (LocalExec killpg TERM->KILL, guards 0/1 in the shell fallback), no new kill code. Logs: per-session tempfile dir harness-jobs-* on the harness machine (outside the protected data dir), removed with the manager; in-memory 4 KiB tail feeds the notice. Background jobs have no timeout unless the call passes one (not capped by bash_max_timeout). bash_jobs tool (list/output/stop) is registered in ToolSet::standard and ToolSet::select adds it right after bash, so built-in YAML/TOOL_NAMES are unchanged. Notices: harness_agent::job_notices::JobNotices RequestHook (TodoNudge pattern) appends one redacted text block per ended job to the unsent last user message (tool results or the next prompt); registered for the top-level loop in core.rs (and, after the review fix, for every child loop; see below). Core::finish calls JobManager::shutdown (quit incl. Ctrl+C quit, /new, /resume, headless end); dropping the manager also kills jobs. New additive event bash_job {call_id, job_id, command, state, exit_code?, duration_ms?, output?} (command/output redacted). TUI: StatusLine folds bash_job into a '⚙ N jobs' indicator registered as a button next to the MCP one (ChatAction::Jobs -> /jobs); new /jobs builtin (harness-agent commands) and crates/harness/src/tui/jobs.rs panel: list with state/elapsed/command/log and buttons output jN / stop jN / refresh; output view reads the log tail without moving the model's read offset. Limits: ToolsConfig bash_max_jobs=4, bash_job_output_bytes=32 MiB. Note: [tools] in harness.ini is not parsed by harness-config today (core uses ToolsConfig::default for all [tools] keys, pre-existing gap); the new keys are documented next to the existing ones in docs/tools.md and doc-2 like them.
Tests: harness-tools jobs::tests (9: immediate return + exit handed out once + events, read-new offsets, stop kills group, shutdown and drop leave no process and remove the log dir, max jobs, output cap, timeout + cwd isolation, tool errors, formatting); harness-agent tests/job_notices.rs (notice on the next request once, redacted, recorded with the request, cache prefix unchanged, bash_job events; job ending between turns lands on the next prompt); harness-tui status + chat view indicator (click + Shift+Tab/Enter, Esc); harness tui::tests jobs_panel_lists_opens_and_stops_background_jobs (driver panel, output view, real App Right+Enter and click on stop, stop kills, finish_core kills the rest); harness-remote tools_remote background_jobs_run_and_stop_through_the_helper and background_jobs_work_on_the_shell_fallback. Updated expectations for intended additions: commands builtin list (+jobs), dispatch standard tool names (+bash_jobs); golden request snapshots (coder/researcher/reviewer) reviewed: only bash description/schema and the new bash_jobs spec changed.
Validation: cargo nextest run --workspace --no-fail-fast 1831 passed, 10 skipped; cargo test --workspace --doc ok; cargo clippy -D warnings clean for harness-tools/agent/events/tui/remote/harness; cargo fmt --all --check clean; scripts/hermetic-guard.sh nextest run -p harness -p harness-eval -p harness-tools -p harness-agent 447 passed, nothing written to HOME/XDG; no leftover harness-jobs-* dirs or sleep processes after the runs. Initial-context CI test: 2270/4000 tokens (it measures named tools only; bash_jobs adds roughly 100 tokens to real coder requests). CHANGELOG: Added entry and Defaults entry with fingerprint 45850933b6b6 (unchanged by this task; tool descriptions are not part of the fingerprint; integrator reconciles concurrent entries).

Review fix (TASK-153 findings). Both blocking findings were real: (1) each subagent / workflow agent step got a fresh ToolSet from tools_factory and so its own JobManager (own j1.., no notice, not in /jobs, never shut down; remote steps likewise); (2) drive() returned before emitting the final Job event when the manager was gone, and the status line keyed jobs by job_id only.
Changes: harness-tools jobs: JobOwner (one per BashTool, released on drop), start_owned, take_finished_for(owner, orphans), scoped() handles whose shutdown stops only their own jobs, same_jobs(); drive() always emits the final ToolEvent::Job (built from the start info when the manager is gone: status stopped, bytes counted, no log path). BashTool::with_jobs/job_owner; ToolSet::standard_with(StandardOptions{base_env, jobs}) and job_owner(). core.rs creates one session JobManager and passes it to base tools and tools_factory (new jobs param); workflow::Setup.jobs (TUI passes the session manager, CLI None) -> Built.jobs (scoped in the TUI) shut down when the run ends (CLI and TUI) before the run log closes; remote agent steps reuse base.jobs(). JobNotices::agent(&tools) is hooked into every child loop in SubagentRuntime::run_child; the top-level uses JobNotices::session (own jobs + jobs whose owner is gone). harness-tui StatusLine keys running jobs by (call_id, job_id). Docs: docs/tools.md, CHANGELOG Added entry reworded (no Defaults change: prompts, tool descriptions and ini defaults unchanged; golden snapshots unchanged).
New tests: harness-tools jobs::tests dropping_the_manager_still_reports_the_end_of_a_running_job, shared_managers_hand_each_agent_its_own_jobs, orphaned_jobs_are_reported_to_the_session_agent, a_scoped_handle_shuts_down_only_its_own_jobs; harness-tui status jobs_indicator_keys_jobs_by_call_and_id; harness tui::tests subagent_jobs_are_reported_listed_and_stopped_with_the_session (spawned worker starts two jobs: notice in the worker's request, /jobs lists j1+j2, stop j2 kills the process, the top-level's next request reports j2); harness tests/commands.rs workflow_agent_steps_get_job_notices_and_their_jobs_end_with_the_run (wf run: notice in the step's request, j2 stopped at run end, process gone). Remote agent steps' sharing is covered by code path only (base.jobs()); no new remote-step test.
Validation: cargo nextest run -p harness-tools -p harness-agent -p harness-workflow -p harness-tui -p harness -p harness-remote -p harness-eval: 1088 passed, 4 skipped; clippy -D warnings clean for those crates; cargo fmt --all --check clean; no leftover harness-jobs-* dirs or sleep processes.

Integration (TASK-151..155, 2026-10-23): workspace green on the combined tree with TASK-149/150 also Done: cargo fmt --all --check ok; cargo clippy --workspace --all-targets --locked -D warnings ok; cargo nextest run --workspace --locked --profile ci 1838 passed / 10 skipped (golden_requests and prompt snapshots included); cargo test --workspace --doc ok; cargo deny check ok; scripts/hermetic-guard.sh nextest -p harness -p harness-eval 216 passed, nothing written to HOME/XDG. CHANGELOG Defaults: the provisional TASK-152/153/155 entries are merged into one reconciled entry (also naming TASK-151's cache_ttl and TASK-153's [tools] job limits) with final fingerprint 45850933b6b6; defaults_changelog passes. README gains a Focused tools highlight and the harness-tools crate row. 'Passes in CI' is pending until the orchestrator pushes; nothing is committed.

Orchestrator follow-up after a live Opus 5.5 session, where the model polled with 'sleep 18; cat log' because it needed the result: added bash_jobs action wait {id, timeout (default 60, max 600)} returning new output and state, marking the exit reported so no notice repeats it; tools.md layer asks for wait instead of sleep. Unit test wait_returns_the_exit_and_its_notice_is_not_repeated. Rerun live: model used run_in_background then bash_jobs wait, no sleep. Also fixed the flaky workflow_agent_steps_get_job_notices test (fixed sleeps and a pid-file race replaced by file waits; 24 parallel runs clean).

Committed in 58b1512; CI green on Ubuntu and macOS: https://github.com/thowd22/harness/actions/runs/40009831788.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
bash gains run_in_background: the command runs as a session-owned job (own process group, any Exec: local, sandbox, remote helper, shell fallback) and the call returns at once with a job id and log path. When a job ends, the JobNotices request hook appends one redacted notice (command, exit status, run time, last 20 lines, log path) to the agent's next unsent request, so the model no longer polls with long sleeps. A new bash_jobs tool (list/output/stop) comes automatically with bash. Jobs stop when the session ends (Core::finish: quit including Ctrl+C quit, /new, /resume, headless end) and when the manager is dropped. At most 4 run at once and each log holds up to 32 MiB. A new bash_job event drives the TUI: a '⚙ N jobs' status-line button and a /jobs panel listing jobs with elapsed time, with output/stop/refresh buttons that work by mouse and keyboard. Verified by unit, integration, remote (helper + shell fallback) and TUI tests. Results: workspace nextest 1831 passed, clippy and fmt clean, hermetic guard ok, no orphan processes. Golden snapshots were reviewed and docs updated (tools, events, commands, tui-views, doc-2, CHANGELOG). Committed in 58b1512; CI green on Ubuntu and macOS: https://github.com/thowd22/harness/actions/runs/40009831788.
<!-- SECTION:FINAL_SUMMARY:END -->
