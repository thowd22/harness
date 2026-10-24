# Done-gating

The harness, not the model, decides whether work is verified.

When a `Verifier` is configured (`[verify] command`, or the agent's
`verify:`) and the model ends its turn, the agent loop runs it:

- `LoopConfig::verify_when` sets when it runs. `Always` (the default) runs it
  every time the model ends its turn. `OnChanges` runs it only when the run
  changed files: a successful mutating tool call (anything that is not
  read-only, bash included), except a bash command that, according to the
  change ledger, changed nothing or only
  [generated files](hygiene.md#generated-files) such as `__pycache__/*.pyc`
  (the ledger's bash reports `ToolEvent::WorktreeChanges`, which the
  dispatcher keeps as `CallOutcome::changed`). An auto-detected verify
  command uses `OnChanges`.
- When verification fails, the failure goes back to the model as a user
  message with the command, its exit code and the end of its output. The
  model then gets another turn to fix it. This repeats at most
  `LoopConfig::max_fix_attempts` times (`[verify] max_fix_attempts`). The
  default is 0, which means verify once and report.
- Each verifier run is recorded as `verify_started` / `verify_finished` on
  the agent node.

The final status is `LoopOutcome::verification`:

| Status | Meaning | Node status | Headless exit code |
|---|---|---|---|
| `verified` | the last verify run passed | `succeeded` | 0 |
| `unverified` | no verifier ran (none configured, no changes, or the run stopped early) | from the stop reason | from the stop reason |
| `failed` | still failing after every fix attempt | `failed`, error `verify_failed: ...` | 3 |

The map reads `last_verify_passed` from the node to tell verified and
unverified apart. `StopReason::from_node_finished` maps `verify_failed` back
to an end of turn, so resumed sessions keep the right stop reason.
