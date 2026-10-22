# Change hygiene and verification

harness keeps a record of everything an agent changes and checks the result
with external tools, instead of trusting the agent's own account. This page
covers the change ledger, checkpoints and `/undo`, scope, the hygiene report,
the git staging guard, the verify gate, the bug-fix proof and the
independent reviewer. The code lives in the `harness-hygiene` crate, except
the staging guard, which is in `harness-policy` (`harness_policy::staging`).

No new `harness.ini` keys are introduced; the verify gate uses the existing
`[verify]` section (see the harness.ini reference, doc-2).

## Change ledger

Every file an agent creates, modifies or deletes is recorded per node:

- **File tools** (`write`, `edit`) report each change directly, with
  before/after hashes and a unified diff.
- **bash** changes are found by snapshotting the worktree before and after
  each command and comparing them. Files ignored by `.gitignore` and the
  `.git` directory are not watched; hidden files are. Small text files keep
  their content in the snapshot, so bash edits get a diff too.

Each entry is written to the run's event log as a `file_changed` event on the
node that made it, so it can be replayed later and appears in node detail
(the node's "files changed" count and list). In node detail a change reads
`+ path` (created), `~ path` (modified) or `- path` (deleted), followed by
what made it (`write`, `edit`, `bash`).

## Checkpoints and `/undo`

Before the first mutating tool call of every turn, harness snapshots the
worktree into a **shadow git repository** under the data dir
(`<data_dir>/checkpoints/<project-hash>.git`). It is a separate git dir
pointed at your worktree; your own repository is never touched:

- your index, branches, stash, HEAD, hooks and config stay exactly as they
  were (git runs with an explicit `--git-dir`, no inherited `GIT_*`
  variables and no global or system config);
- the project's `.git` directory is never part of a snapshot;
- files ignored by `.gitignore` are neither snapshotted nor touched by a
  restore.

`/undo` restores the worktree to how it was before the most recent agent
turn. Repeating `/undo` walks further back. Any node's checkpoints can be
restored from the map. Every restore first takes a safety checkpoint, so a
restore can itself be undone. Nested git repositories are recorded only as
references, not by content.

## Scope

A scope is a list of path globs the task is expected to touch, such as
`src/**` and `tests/**`. It comes from (most specific first):

1. the session (set by the user),
2. the workflow step's `scope:`,
3. the agent's `scope:`.

`*` does not cross `/`; `**` does; a trailing `/` means everything below.

The model sees the scope as the areas it works in, never as a list of
prohibitions. An edit outside the scope is not refused:

| Mode | Edit outside scope |
| --- | --- |
| `ask`, `auto-edit` | asks for approval, naming the scope |
| `auto` | goes ahead and is flagged in the hygiene report |

Changes made through bash are checked after the command, by the ledger.

## Hygiene report

At the end of a node (and before it is reported done), the report lists
every file the node changed, in one or more categories:

| Category | Meaning |
| --- | --- |
| in scope / out of scope | against the node's scope (no scope: everything is in scope) |
| new untracked | created and not tracked by your git |
| temp/debug | created and looks like scratch work: inside `tmp/`, `temp/` or `scratch/`; `*.tmp`, `*.bak`, `*.orig`, `*.rej`, `*.swp`, `*.log`, `*~`; `out.txt`, `a.out`, `nohup.out`; names starting with `tmp`, `temp`, `scratch`, `debug` or `foo` followed by `_`, `-`, a digit or the extension |
| whitespace-only | the file changed, but only in whitespace |

From the report you can revert selected files (back to the node's first
checkpoint; files the node created are removed) and remove the temp/debug
files in one step.

The **hygiene score** is the share of changed files with no problem
(out-of-scope, temp/debug or whitespace-only), from 0 to 100; a node that
changed nothing scores 100. It is appended to the run's `metrics.jsonl` as
the metric `hygiene.score`, with the counts behind it.

## Git staging guard

`git add -A`, `git add .`, `git add --all`, `git add :/` and
`git commit -a` / `-am` / `--all` sweep every change in the worktree into a
commit, temp files and unrelated edits included. In every mode these
commands ask for approval, with a message telling the agent to stage
explicit paths instead (`git add src/parser.rs tests/parser.rs`, then
`git commit` without `-a`). The guard recognises the usual spellings
(`git -C dir add .`, `/usr/bin/git add -A`, `git add -fA`, inside
`bash -c` or `sudo`).

Staging explicit paths is unaffected and follows your normal policy and
mode. To allow a sweeping command anyway, name it explicitly in
`[policy] bash.allow` (for example `git add -A*`); broad patterns such as
`git add *` do not count. The guard only tightens: a deny rule still denies.

## Verify gate

The verify command comes from, most specific first: the workflow step's
`verify:`, the agent's `verify:`, `[verify] command`, or auto-detection:

| File in the project root | Command |
| --- | --- |
| `Cargo.toml` | `cargo test --workspace` |
| `go.mod` | `go test ./...` |
| `package.json` | `npm test` |
| `pyproject.toml`, `setup.py` | `pytest` |
| `Makefile` | `make test` |

With `[verify] baseline = true` (the default) the command runs once before
the agent's first edit. When the agent finishes, it runs again. harness
reads the exit status and, when the output has test results, the set of
failing tests. Machine-readable output is preferred (libtest/nextest JSON
lines, `go test -json`); plain `cargo test` and pytest summaries are
understood too.

The result separates **newly failing** tests (failing now, not in the
baseline) from **pre-existing** failures and from tests the change fixed:

```
verify failed: 1 newly failing test(s), 1 pre-existing
  newly failing: parser::header_overflow
  pre-existing: net::flaky_timeout
  fixed: parser::header_empty
```

Without a baseline, or when the output has no test results, only the exit
status counts. Each run is logged as `verify_started` / `verify_finished`
events with the full output.

## Bug-fix proof

The `bugfix-proof` workflow step proves that the fix is what makes the test
pass. It runs the test in four phases, each a visible child node with its
own log:

1. **original code**: the fix is taken out; the test must fail;
2. **with fix**: the fix is put back; the test must pass;
3. **fix reverted**: the fix is taken out again; the test must fail;
4. **restore fix**: the fix is put back.

The fix is the set of files changed since the checkpoint taken before the
fix step. Paths matching the step's keep globs (`tests/**` by default, where the
reproducing test lives) stay in place throughout. The step's `proven` output is true only when every
phase met its expectation. The fix is always restored: after the last
phase, on error, when the run is cancelled, and when the step is
interrupted.

## Independent review

A reviewer agent, ideally on a different model, receives only the task, the
scope and the diff. It never sees the implementer's conversation, reasoning
or tool output. It grades the diff on:

- **correctness**: does it do what the task asks, including edge cases;
- **unrequested changes**: unrelated edits, drive-by refactors, stray files;
- **over-engineering**: abstractions or options nobody asked for;
- **dead or compatibility code**: unused, commented-out or shim code;
- **test adequacy**: tests cover the change; no weakened or skipped tests.

It answers with a structured verdict (`approve` or `request_changes`), a
grade and notes per criterion, and findings with severity, file and line. An
approval with a failed criterion or a blocker finding counts as
`request_changes`. Workflows loop on it, for example:

```yaml
- id: review
  agent: reviewer
  model: opus
  loop: { until: "{{ steps.review.outputs.verdict == 'approve' }}", max_iterations: 3, retry_step: fix }
```
