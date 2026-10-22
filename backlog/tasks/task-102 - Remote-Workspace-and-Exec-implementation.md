---
id: TASK-102
title: Remote Workspace and Exec implementation
status: Done
assignee:
  - '@remote'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 20:51'
labels:
  - remote
  - tools
milestone: m-10
dependencies:
  - TASK-101
priority: high
type: feature
ordinal: 102000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Agents' tools must run remotely without changes while model calls and credentials stay local.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Remote implementation passes the shared Workspace and Exec trait tests
- [x] #2 Policy, redaction and change ledger work identically for remote tools
- [x] #3 Events are tagged with host
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
1. RemoteWorkspace and RemoteExec over RemoteClient RPCs. 2. Env policy (scrub secrets, never forward local host vars). 3. Run harness-tools conformance suites in-process and over the helper binary. 4. Prove tools (write/edit/read/bash) behave identically for ledger, redaction and policy. 5. Host-tagged audit events.

6. Placed-node execution (AC#3): harness_remote::node::{HostConnect, SshHosts, RemoteNode} (connect, push, tools, pull, gated apply, cleanup, node-tagged remote_op/remote_connection); harness_agent ChildHost (remote tools, host gate, node_started.host, inherited by subagents); harness_workflow::remote::RemoteSteps wrapping agent/shell/verify runners (proof/dream local-only); agent steps inherit their agent's runs_on; wired in harness wf build for [remote.*]; e2e tests over the helper on a local pipe.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
workspace.rs:
- RemoteWorkspace::new(client, root) implements harness_tools Workspace over fs.* RPCs. Remote paths are made absolute with the helper's home, and payloads that contain a guarded secret are refused before encoding.
- RemoteExec::new(client, RemoteEnv) implements Exec. It pre-registers a job sink so no output is lost, builds ExecHandle::new from the client channels, and sends exec.kill on handle kill or drop.
- RemoteEnv scrubs secrets with harness_tools::bash::EnvScrubber and never forwards local host-specific variables.

Tests:
- tests/conformance.rs runs harness_tools::conformance::{workspace_suite, exec_suite} against both an in-process helper and the real helper binary over a pipe (AC#1).
- tests/tools_remote.rs:
  - change_ledger_and_stale_reads_match_local: identical FileChanged events and diffs from write/read/edit, and the same StaleRead error, local vs remote.
  - redaction_applies_to_remote_output_like_local: harness_events Redactor output is identical.
  - policy_decisions_do_not_depend_on_where_tools_run.
  - every_remote_operation_is_audited_with_its_host. (AC#2)

REMAINING (AC#3, DoD#1): "events are tagged with host" holds only inside harness-remote. Every AuditEvent and ConnectionEvent carries the host, but nothing reaches the run event log yet. Two cross-crate pieces are needed:
(a) harness-events adds e.g. EventKind::RemoteOp{host, op, target, ok, error, duration_ms} and EventKind::RemoteConnection{host, state}.
(b) harness-agent sets NodeStarted.host to the [remote.<name>] for remote nodes, implements harness_remote::audit::AuditSink over RunRecorder (pass it in ClientOptions.audit), and forwards RemoteClient::subscribe() events.
Then add a test and check AC#3 and DoD#1.
cargo test -p harness-remote: 91 tests pass (64 unit, 27 integration in tests/{conformance,failures,install,sync,tools_remote}.rs), 1 #[ignore] real-sshd test; cargo test -p harness --test remote_agent: 2 pass; cargo clippy -p harness-remote --all-targets -- -D warnings clean.

Integrator update: harness-events has EventKind::RemoteOp{host,...} and RemoteConnection{host,state,detail}; harness_remote::audit::RecorderAudit and record_connection write them to a run's event log (test: harness-remote tests/run_log.rs). REMAINING (AC#3, DoD#1): no production run executes tools remotely yet: workflow steps placed by runs_on run locally (documented in docs/workflows.md), and the TUI only uses the remote client for /remote test. A remote-aware StepRunner/session must build RemoteWorkspace/RemoteExec tools with ClientOptions::with_audit(RecorderAudit), set NodeStarted.host, and add an end-to-end test; then check AC#3 and DoD#1.

AC#3 completed (remote lane, this pass):
- harness-remote src/node.rs: HostConnect trait + SshHosts (OpenSSH, connect_with_upgrade, rsync/git pusher from [remote.<name>] sync, cleanup policy); RemoteNode::open records remote_connection connecting/connected/failed on the node, audits every RPC via RecorderAudit(node), forwards client connection events with audit::record_connection_on, pushes to <workdir>/<project-key>/<run-id>-<node>, creates <run dir>.artifacts outside the pulled tree; finish() pulls, applies through ChangeGates (all or nothing, audited as sync.apply), cleans up (artifacts dir too), closes, records disconnected. RemoteRun::project_dir() added.
- harness-agent: ChildHost {name, root, tools: HostTools, gate: Option<GateFactory>} on ChildRequest.host; run_child swaps the base tools, uses the host gate, sets NodeStarted.host (also LoopConfig.host for the loop's own node_started), relativizes file-change paths to the host root; SpawnAgentTool passes the host to subagents. ToolSet::fill_from keeps non-standard tools (memory/todo) local.
- harness-workflow src/remote.rs: RemoteSteps {hosts, recorder, local_root, RemoteEnv, SyncGates, HostGates} with agent()/shell()/verify()/local_only(); RemoteStepRunner delegates when req.host is None, otherwise opens a node, runs the placed runner, copies declared shell artifacts back, applies with ApplyOptions{approved:true} (ask already settled at tool-call time; denials still block), saves remote-changes.patch and fails when not applied. AgentStepRunner::on_host. Schema: CheckContext.agent_runs_on so an agent step without runs_on is placed like its agent.
- harness crate: remote::steps(StepWiring) builds SshHosts, sync gates (local PolicyGate + HygieneGate with scope) and per-host agent gates (run policy + host policy.bash overlay, shared grants/broker, capped at agent mode); workflow::build wraps runners when [remote.*] exists.
- Tests: harness-agent subagents::tests::a_child_on_a_host_uses_its_tools_tags_its_node_and_passes_the_host_on; harness-remote tests/node.rs (real helper binary over a local pipe: tools, artifacts, apply, cleanup, node-tagged remote_op/remote_connection; failed connect recorded); harness-workflow tests/remote_steps.rs (in-process helper over a pipe: shell+agent+verify steps on gpu-box, local downstream step sees applied changes, host on step/agent nodes, remote ops only on placed nodes, workspace-relative file changes; gate rejection saves patch and keeps run dir; proof on host fails clearly); schema::tests::agent_steps_without_runs_on_are_placed_like_their_agent; harness remote::tests::agents_on_a_host_get_its_tighter_policy.
- Docs: docs/workflows.md (What runs on the host), docs/remote.md (Running workflow steps on a host, security notes), doc-6 decision, CHANGELOG Added entry.
- Not verified live: no real sshd was used (SshHosts is exercised only through compile and the existing #[ignore] real-sshd test path); OpenSSH transport itself is covered by TASK-100 tests.
- Validation: cargo test -p harness-agent -p harness-remote -p harness-workflow all pass; cargo test -p harness --bin harness remote pass; clippy -D warnings clean for harness, harness-agent, harness-remote, harness-workflow; cargo test --workspace --no-fail-fast: only harness-eval defaults_changelog fails, caused by a concurrent harness.ini default change in crates/harness-config (providers lane), not by this task.

Review fixes (remote lane):
- Finding 1 (env forwarding) CONFIRMED and fixed. crates/harness/src/workflow.rs passed scrubber.scrub(std::env::vars()) to placed shell/verify steps, and agents on a host got BashTool::new with base_env = std::env::vars(), so non-harness local secrets (GITHUB_TOKEN, AWS_*, ...) reached the host. Now: the wiring passes no local env to RemoteSteps::shell/verify (param renamed to `declared`, documented as step-declared variables only); placed agents use new ToolSet::standard_with_bash_env(parts, Vec::new()); RemoteExec requests the remote login basics (HOST_ENV_FROM_REMOTE) for every clean-env process so remote bash still has HOME/PATH/USER/SHELL/LANG. Test: harness-workflow tests/remote_steps.rs the_local_environment_is_not_sent_to_the_host (tees the wire: every exec.start for shell, agent bash and verify carries only ARTIFACTS_DIR/declared/non-interactive vars; agent `env` on the host shows PATH/HOME/TERM and none of the local-only variables). Confirmed it fails with the old bash base env (AI_AGENT was sent to the host by bash). Limitation: the test cannot set a variable in the test process (set_var is unsafe), so it uses the variables the test process already has (CARGO_* etc.); the in-process helper inherits the test process env for non-clean shell steps, so for shell/verify the check is on the wire, and the production wiring change in crates/harness is covered by review, not a test.
- Finding 2 (run dir reuse on retry) CONFIRMED and fixed. RemoteNode::open takes the attempt; run id is <run-id>-<node>-a<attempt> (artifacts dir follows). Test: a_retried_step_gets_a_fresh_run_dir_and_no_stale_artifacts (attempt 1 writes report + junk.txt and fails; attempt 2 sees no junk.txt, writes no report, and the step fails with 'did not produce its declared artifact'; the -a1 dir is kept with junk.txt). Confirmed it fails with the old run id.
- Docs: docs/remote.md (run dir name, env in steps and Security), docs/workflows.md, doc-6, CHANGELOG Added entry.
- Validation: cargo test -p harness-remote -p harness-workflow -p harness-agent pass; cargo test -p harness --bin harness pass; clippy -D warnings clean for harness, harness-agent, harness-remote, harness-workflow; cargo test --workspace --no-fail-fast: only harness-eval defaults_changelog fails (defaults fingerprint dbcc02b5b7e2: built-in prompts/agents/workflows/harness.ini changed by another lane; this change touches none of them). No live SSH host used.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Placed workflow steps now run their tools on the remote host. harness_remote::node::RemoteNode (connect via HostConnect/SshHosts, push, RemoteWorkspace/RemoteExec, pull, policy+hygiene-gated apply, cleanup) records host-tagged remote_op and remote_connection events on the step node; harness_workflow::remote::RemoteSteps wraps agent, shell and verify runners (proof/dream fail clearly when placed); agents get remote read/write/edit/bash plus the host's policy overlay through harness_agent ChildHost, with node_started.host on step, agent and subagent nodes; model calls and credentials stay local. Agent steps inherit their agent's runs_on. Wired into harness wf build for [remote.*]. Verified with tests/remote_steps.rs (workflow e2e over an in-process helper on a pipe), harness-remote tests/node.rs (real helper binary over a local pipe), the subagent host test, the schema test and the harness host-gate unit test; clippy clean. No live SSH host was used.

Review fixes: placed shell/verify steps and agent bash on a host no longer send the local environment (host login env plus ARTIFACTS_DIR/non-interactive vars), and each retry attempt gets its own remote run dir (<run-id>-<node>-a<attempt>), so retries never see a failed attempt's tree or artifacts. Covered by remote_steps.rs tests the_local_environment_is_not_sent_to_the_host and a_retried_step_gets_a_fresh_run_dir_and_no_stale_artifacts.
<!-- SECTION:FINAL_SUMMARY:END -->
