# Core tools: read, write, edit, bash, grep, glob

harness gives the model four small core tools and two read-only search
tools (`grep`, `glob`) that the built-in agents enable. They all go through one
`Workspace`/`Exec` layer, so they behave the same on your machine and on a
remote host, and every file change and command passes one place where policy
and the change ledger can see it.

## read

```json
{"path": "src/main.rs", "offset": 120, "limit": 80}
```

- Output is numbered like `cat -n` (`   120\tfn main() {`).
- `offset` is the 1-based first line (default 1); `limit` the number of lines.
- One call returns at most `read_max_lines` lines and `read_max_bytes` bytes;
  lines longer than `read_max_line_chars` are cut. When more remains, the
  output ends with `[Showing lines 120-199 of 850. To read more, call read with offset=200.]`.
- Files larger than `read_max_file_bytes` are refused with a suggestion to use
  `rg`, `head`, `tail` or `sed -n`.
- Binary files (a NUL byte in the first 8 KiB) are described, not dumped.
- PNG, JPEG, GIF and WebP images are sent to the model as images when the
  current model accepts image input (up to 5 MiB); otherwise read says the
  model cannot view images.
- `path` may also be a `spill:<id>` pointer from a truncated output (see
  below).
- Every read records a hash of the whole file. write and edit use it to detect
  stale reads.

## write

```json
{"path": "src/new.rs", "content": "..."}
```

- Creates the file (and parent directories) or replaces its whole content.
- Overwriting an existing file requires that the model has read it and that it
  has not changed since. Otherwise write fails and tells the model to read the
  file again.

## edit

```json
{"path": "src/main.rs", "old_string": "let x = 1;", "new_string": "let x = 2;", "replace_all": false}
```

- Exact string replacement. `old_string` must occur exactly once, unless
  `replace_all` is `true`. An ambiguous match lists the lines it occurs on.
- The file must have been read and must not have changed since.
- When `old_string` is not found, the error says why when it can: read's
  line-number prefixes were copied, or the text only differs in indentation
  (with the line where the near-match starts).
- Files with CRLF line endings are matched and written with CRLF, so the
  model can use the text it sees in read output.

## What every write and edit guarantees

- **Atomic**: the new content goes to a temporary file in the same directory,
  which is then renamed over the target. Readers never see a half-written file.
- **Permissions kept**: an existing file keeps its mode (e.g. `0755` scripts
  stay executable); new files get `0644`. Symlinks are followed, so the link
  stays and its target is updated.
- **Line endings kept**: a CRLF file stays CRLF.
- **Recorded**: each change is reported with a unified diff and before/after
  hashes for the change ledger. The model's own write counts as a read, so
  several edits in a row need no re-read.

## bash

```json
{"command": "cargo test -p mycrate", "timeout": 300}
```

- Runs `bash --noprofile --norc` non-interactively: stdin is `/dev/null`,
  `PAGER`/`GIT_PAGER` are `cat`, `GIT_TERMINAL_PROMPT=0` and `TERM=dumb`.
- `timeout` is in seconds. The default is `bash_timeout` (120) and the maximum
  `bash_max_timeout` (600); larger values are capped, with a note.
- On timeout, cancel or when the command finishes, the command's whole
  process group is killed: SIGTERM (with SIGCONT, so stopped processes can
  exit), then SIGKILL after 2 seconds. Background jobs (`server &`) do not
  outlive the call; use `run_in_background` (below) for those.
- The command runs in its own session with no controlling terminal, so
  anything that opens `/dev/tty` (an ssh or sudo password prompt,
  pinentry-tty) fails at once instead of waiting for input that cannot come.
- Output streams live to the UI. The model gets stdout and stderr combined,
  followed by `[exit code N]` on failure or a timeout/cancel note.
- **Liveness**: while the command runs, its process group is sampled every
  3 seconds for CPU time and live processes (Linux: one pass over
  `/proc`; macOS and other Unix: one `ps -A -o pgid=,stat=,time=` run).
  The samples and the call's timeout go to the UI as `tool_progress`
  events, so the activity line can show a quiet but busy command as
  working and a quiet idle one as quiet (see
  [tui-views.md](tui-views.md#activity-line)). They are not sent to the
  model. Where sampling is unavailable the UI shows elapsed time against
  the timeout.
- The working directory persists between calls in a session: `cd sub` in one
  call applies to the next. If that directory is deleted, the next command
  runs in the project root and says so.
- **Environment scrubbing**: these variables are removed before the command
  starts:
  - provider keys `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`,
    `OPENAI_API_KEY`, `OPENROUTER_API_KEY`;
  - every variable named by an `api_key_env` in `harness.ini`;
  - every `HARNESS_*` variable;
  - any variable whose value contains a resolved secret (8 characters or more).

### Background jobs: `run_in_background`

```json
{"command": "cargo test --workspace", "run_in_background": true}
```

For servers, watchers and long builds or tests. The call returns at once:

```text
[background job j1 started; its output goes to /tmp/harness-jobs-Ab12/j1.log. It keeps running after this call; harness reports its exit code and last output in a later message. Read new output with bash_jobs (action: output, id: j1), stop it with action stop.]
```

- The command starts like any bash call (same environment scrubbing,
  sandbox, host and policy check; its own session and process group) in
  the session directory. A `cd` inside it does not change the session
  directory.
- It has **no timeout** unless the call gives one (`timeout` then applies
  as is, not capped by `bash_max_timeout`).
- Its combined stdout and stderr go to a log file in a per-session
  temporary directory on the harness machine (removed when the session
  ends), up to `bash_job_output_bytes` (32 MiB); later output is counted and
  dropped with a note in the log. The output is not streamed to the chat.
- When it exits, harness appends a notice to the agent's **next request**
  (the unsent tail, so earlier requests and the prompt cache are unchanged;
  also when the job ends between turns, then with the next prompt):

  ```text
  Notice from harness: background job j1 ended (exit code 101) after 4m12s.
  Command: cargo test --workspace
  Last output:
  test result: FAILED. 812 passed; 2 failed
  Full output (1843211 bytes): /tmp/harness-jobs-Ab12/j1.log
  ```

  At most the last 20 lines (2000 bytes) are included, redacted like every
  output. The notice goes to the agent that started the job: a subagent
  or workflow agent step hears about its own jobs while it runs, and a job
  that ends after its subagent has finished is reported to the top-level
  agent. Every agent of the session (subagents, workflow agent steps,
  agents on remote hosts) shares one set of jobs, so ids (`j1`, `j2`, ...)
  are unique in the session and `/jobs` lists and stops all of them.
- At most `bash_max_jobs` (4) jobs run at once; another start fails with a
  message saying how many run and how to stop one.
- **Jobs belong to the session.** Quitting harness (including Ctrl+C
  quit), `/new`, `/resume` and the end of a headless run stop every job
  that is still running: its whole process group gets SIGTERM, then SIGKILL
  after 2 seconds (the same as a cancelled call; remote jobs are killed on
  the host). A workflow run (`harness wf run`, or a workflow started in the
  TUI) stops the jobs its agent steps started when it ends. Interrupting a
  turn does not stop jobs.
- Each start and end is recorded as a `bash_job` event
  ([events.md](events.md)); the TUI shows running jobs in the status line
  and in `/jobs` ([tui-views.md](tui-views.md#status-line)).

### bash_jobs

Offered with `bash` (agents that list `bash` get it right after it):

```json
{"action": "list"}
{"action": "output", "id": "j1"}
{"action": "wait", "id": "j1", "timeout": 120}
{"action": "stop", "id": "j1"}
```

- `list`: every job of the session with its state (`running`, `exit code
  N`, `killed by signal N`, `timed out`, `stopped`, `lost`), run time,
  command and log file.
- `output`: the job's output written since the last `output` call (at most
  1 MiB; older unread output is skipped with a note), through the output
  budget like any result, then the job's state.
- `wait`: blocks until the job ends or `timeout` seconds pass (default 60,
  at most 600), then returns its new output and state. Use it instead of
  `sleep` when the result is needed; an exit seen through `wait` is not
  repeated as a notice.
- `stop`: kills the job's whole process group and waits for it to end.

Ended jobs are kept for `list` and `output`; beyond 32 ended jobs the
oldest reported ones are forgotten and their logs removed.

## grep

```json
{"pattern": "fn connect", "path": "src", "glob": "*.rs", "output_mode": "content", "-C": 2}
```

A ripgrep-style regex search, so the model searches without bash.

- `pattern` is a Rust regex; `-i` ignores case, `multiline` lets `.` match
  newlines (a match may span lines), and `^`/`$` match at line ends.
- `path` is a file or directory (default the project root). `glob` filters
  files (`*.rs` matches at any depth; a pattern with `/`, such as
  `src/**/*.{ts,tsx}`, is relative to `path`). `type` takes ripgrep file
  types (`rust` or `rs`, `py`, `js`, `ts`, `go`, `md`, ...).
- `output_mode`: `files_with_matches` (default; matching paths sorted by
  path), `content` (`path:line:text`, context lines as `path-line-text`,
  groups separated by `--`; `-n` line numbers default on, `-A`/`-B`/`-C`
  context up to 50 lines) or `count` (`path:N` matching lines per file and
  a total).
- `head_limit` (default 250, `0` for no limit) and `offset` page through
  the entries (paths, count lines or output lines); the output ends with
  the `offset` to continue from. Lines are cut at 400 characters.

## glob

```json
{"pattern": "src/**/*_test.py", "path": "."}
```

- Lists files matching a globset pattern relative to `path` (default the
  project root): `*` stays in one directory, `**` crosses directories,
  `{a,b}` picks alternatives.
- Newest first (modification time), at most 200 paths; the output says how
  many more matched.

## What grep and glob share

- **Ignore rules**: `.gitignore`, `.ignore` and git's global excludes are
  honoured (also outside a git repository), hidden files are searched and
  `.git` is always skipped. A file named directly in `path` is searched even
  if it is ignored. grep skips binary files (a NUL byte in the first 8 KiB)
  and files over 32 MiB (it says how many it skipped).
- **Policy**: both are read-only, so several calls in one turn run in
  parallel and they never ask for approval. Their `path` is checked like a
  `read` of that path (a protected directory is denied), and protected
  files (`paths.protected`, harness configuration and data, `.git/hooks`)
  are left out of their results. Results do not count as reading a file for
  write and edit.
- **Budget**: the output goes through the spill budget below like bash
  output.
- **Remote hosts**: the search runs on the host through the helper's
  `fs.find` method ([remote.md](remote.md)). With a helper that predates it,
  glob falls back to `fs.glob` (sorted by path) and grep tells the model to
  use bash `rg` or `grep` instead.
- Patterns must be relative and must not contain `..`; put the directory in
  `path`.

## Output budget and spill files

Outputs larger than `spill_over_bytes` (default 16000 bytes, about 4000
tokens) are not sent to the model in full. The model gets the first
`spill_head_lines` and last `spill_tail_lines` lines with a notice between
them:

```
[... output truncated: 4920 of 5000 lines (21400 of 23893 bytes) not shown. Full output saved as spill:3f2a9c0d1e7b4a55; page through it with the read tool, e.g. path="spill:3f2a9c0d1e7b4a55" offset=41. ...]
```

The full output is kept in the session's blob store, and the model can page
it with `read` like any file. Each spill is recorded as an event with the
original and kept sizes and the estimated tokens saved (bytes / 4).

## Configuration: `[tools]` in harness.ini

| Key | Default | Meaning |
|---|---|---|
| `read_max_lines` | 2000 | Most lines one read returns |
| `read_max_bytes` | 32000 | Most bytes one read returns |
| `read_max_line_chars` | 2000 | Longer lines are cut in read output |
| `read_max_file_bytes` | 67108864 | Larger files are refused by read |
| `bash_timeout` | 120 | Default bash timeout, seconds |
| `bash_max_timeout` | 600 | Largest timeout a call may ask for, seconds |
| `spill_over_bytes` | 16000 | Outputs larger than this are spilled |
| `spill_head_lines` | 40 | Lines kept from the start of a spilled output |
| `spill_tail_lines` | 40 | Lines kept from the end of a spilled output |
| `bash_max_jobs` | 4 | Background bash jobs that may run at once |
| `bash_job_output_bytes` | 33554432 | Bytes of output kept in a background job's log |

```ini
[tools]
bash_timeout = 300
spill_over_bytes = 24000
```

## For implementers

- `harness_tools::workspace::Workspace` and `harness_tools::exec::Exec` are
  the host traits. A new implementation (e.g. remote over SSH) must pass
  `harness_tools::conformance::workspace_suite` and `exec_suite`.
- Platform: Unix only (process groups and permission bits).
