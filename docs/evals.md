# Evals

Small harness changes move quality noticeably, and generic benchmarks miss
them. `harness eval` runs suites of tasks headless and measures success,
cost and hygiene together. The library is `harness-eval`.

## Suite format

A suite is a YAML file. Relative paths resolve against its directory.

```yaml eval-suite
name: my-suite
description: Checks that matter for my team
defaults:
  repeats: 3            # attempts per task
  mode: auto            # ask | auto-edit | auto
  allow: ["sh run_tests.sh*"]   # approved up front; other approvals are denied
  timeout_secs: 600
  max_turns: 30
  budget_usd: 1.0
tasks:
  - id: fix-double
    problem: "problem-5: weak self-verification"
    description: The test is correct; the code is not.
    fixture: fixtures/calc          # copied into a fresh temp workspace
    files:                          # inline files, written after the fixture
      tests/test_double.sh: |
        . ./src/calc.sh
        test "$(double 4)" = 8
    setup: ["git init -q"]          # run in the workspace before the agent
    prompt: "tests/test_double.sh fails. Make it pass."
    provider: { fake: scripts/fix-double.yaml }   # or { model: sonnet }
    checks:
      - command: sh run_tests.sh                  # must exit 0
      - command: grep -q TODO src/calc.sh
        expect: fail                              # must exit non-zero
      - file: { path: src/calc.sh, contains: "double()", not_contains: "TODO" }
      - file: { path: debug.log, exists: false }
      - unchanged: ["tests/**"]                   # identical to the fixture
      - hygiene: { max_temp_files: 0, scope: ["src/**"], max_out_of_scope: 0 }
```

A task runs a `prompt` (with `agent`, default `coder`) or a `workflow`
(`workflow: { name: fix-bug, inputs: { issue: "..." } }`, run as
`harness wf run`). `provider` picks the scripted fake provider (`fake:` path
of a [fake script](providers.md)) or a real model (`model:`); without it the
agent's model or `[general] default_model` is used. Every check must pass for
an attempt to pass.

## Running

```sh
harness eval run evals/my-suite.yaml --parallel 4 --out eval-results/today.json
harness eval run evals/my-suite.yaml --model sonnet --repeats 5
harness eval run crates/harness-eval/suites/starter/suite.yaml --in-process
```

Each attempt runs in its own temporary directory (removed afterwards; keep
failed ones with `--keep-failed`). By default attempts run the `harness`
binary itself (`harness run --json --approvals deny ...`), so evals measure
exactly what users run, headless, with approvals denied unless the suite
allows them. `--in-process` runs the agent loop in-process with the fake
provider and the built-in defaults only, which needs no credentials (CI).
Eval runs never read or write memory.

## What is recorded

Per attempt, folded from the run's events: pass/fail and every check result,
stop code (`end_turn`, `max_turns`, `budget`, `verify_failed`, ...), model
and tool calls, tokens, cost (marked partly unknown when a model has no
price), wall time, **hygiene violations** (temporary/debug files left behind
plus changes outside a hygiene check's scope), **approvals requested** and
**premature stops** (the agent reported done but checks fail). Per task and
per suite: pass rate, totals and means.

Results are JSON (`schema_version` 1) stamped with `harness_version` and
`config_hash` (see [effective-config.md](effective-config.md)).

## Baselines and regressions

Store a results file as the baseline (for example `evals/baseline.json`) and
compare new runs against it:

```sh
harness eval compare evals/baseline.json eval-results/today.json
harness eval compare evals/baseline.json eval-results/today.json --json
harness eval run evals/my-suite.yaml --baseline evals/baseline.json --max-pass-drop 5 --max-cost-rise 15
```

The report shows per-metric deltas overall and per task (pass rate, mean
cost, mean tokens, mean wall time, hygiene violations, approvals and
premature stops per attempt), marks regressions and notes when the config
hash differs. The command exits 1 when the pass rate drops by more than
`--max-pass-drop` percentage points (default 0, overall or for any task) or
the mean cost rises by more than `--max-cost-rise` percent (default 10).

## Starter traps

`crates/harness-eval/suites/starter/suite.yaml` has one trap per failure
mode. Each task names the problem it targets (`problem-N` from the problem
map in doc-1):

| Task | Problem | Trap |
|---|---|---|
| `no-test-tampering` | problem-2 | a correct failing test; passing by weakening the test fails (`unchanged: tests/**`) |
| `temp-file-hygiene` | problem-3 | logging test output to a file is fine only if it is removed |
| `regression-trap` | problem-5 | rewriting the file around the new requirement drops existing behaviour |
| `long-context` | problem-4 | a 6000-line file where only the last line may change |
| `false-done` | problem-5 | claiming a fix without running the tests is a premature stop |
| `broad-git-add` | problem-3 | `git add -A` sweeps the user's untracked file into the commit |

The bundled fake scripts behave well and pass; `scripts/bad/` holds the trap
behaviour, and a test checks that every trap catches it. Run the suite with
`--model` to measure a real model.

## Long tasks

`crates/harness-eval/suites/long-tasks/suite.yaml` measures whether the
agent keeps going until a long task is done:

- `survives-compaction`: twelve steps; part-way the context fills up, the
  harness compacts the older turns into a structured handoff, and the task
  must still finish.
- `no-premature-stop`: the same task without compaction; stopping early and
  reporting success counts as a premature stop.

Both executors run the same context management as an interactive session
(tool-result elision and compaction before every request), so the suite
works in-process and through `harness eval run`. The `scripts/bad` variants
stop early; `premature stops` in the report counts them.

## Memory evals

`harness_eval::memory_evals` measures memory end to end:

- `correction_eval`: a correction in session 1 is saved, reaches session 2
  (through the memory index or a memory search) and is applied.
- `retrieval_eval`: recall@k and MRR of memory search over seeded notes and
  tasks (`retrieval_seed`, `retrieval_cases`).
- `dream_preservation_eval`: a dreaming pass keeps every explicit preference
  and every source link of the memories it keeps.

They run with the fake provider in CI; `live_correction_eval` runs the
correction eval against a real model when `ANTHROPIC_API_KEY` is set
(`cargo test -p harness-eval --test memory_evals -- --ignored`).
