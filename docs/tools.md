# Core tools: read, write, edit, bash

harness gives the model four small tools. They all go through one
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
  outlive the call.
- The command runs in its own session with no controlling terminal, so
  anything that opens `/dev/tty` (an ssh or sudo password prompt,
  pinentry-tty) fails at once instead of waiting for input that cannot come.
- Output streams live to the UI. The model gets stdout and stderr combined,
  followed by `[exit code N]` on failure or a timeout/cancel note.
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
