# Run events, the run store and redaction

Every run harness performs (an interactive session, `harness run`, a
workflow, an eval) is recorded as an append-only event log. The chat view,
map, node detail, run history, replay, `harness runs show` and evals all read
this log, so what you see is exactly what was recorded.

## Where runs are stored

```
<data_dir>/runs/<run-id>/events.jsonl     the event log
<data_dir>/runs/<run-id>/blobs/<sha256>   large payloads
```

`data_dir` defaults to `~/.local/share/harness` (`[general] data_dir`).
Run ids are UUID v7, so sorting directory names sorts runs by start time.

### `events.jsonl`

The first line is a header:

```json
{"schema_version":1,"run_id":"0199a3b2-…","created_at":"2026-10-22T12:00:00Z","harness_version":"0.1.0"}
```

Every following line is one event. All events share an envelope:

| field            | meaning                                                    |
|------------------|------------------------------------------------------------|
| `seq`            | sequence number within the run, starting at 0, increasing  |
| `ts`             | RFC 3339 timestamp                                         |
| `run_id`         | the run                                                    |
| `node_id`        | node the event belongs to (absent for run-level events)    |
| `parent_node_id` | the node's parent, e.g. the agent that spawned a subagent  |
| `type`           | event type, see below                                      |

Event types: `run_started`, `run_finished`, `node_started`, `node_running`,
`node_retry`, `node_finished`, `model_call_started`, `delta`,
`model_call_finished`, `tool_call`, `tool_result`, `approval_requested`,
`approval_resolved`, `file_changed`, `verify_started`, `verify_finished`,
`memory`, `cost` and `error` (plus `tool_output`, `tool_progress`,
`policy_decision`, `remote_op`, `remote_connection`, `tasks`, `mcp_call`,
`mcp_tools_changed` and `bash_job`).

A `file_changed` event has `path`, `change` (`created`, `modified`,
`deleted`), an optional unified `diff`, and `generated: true` when a bash
command wrote a well-known build or test artifact (`__pycache__/*.pyc`,
`target/`, ...; see [hygiene](hygiene.md#generated-files)). Generated
changes are shown but not counted as changes to verify; the field is
omitted when false.

A `bash_job` event records a background bash job (`run_in_background`)
starting and ending, as a full snapshot on the node of the turn that
started it: `call_id` (the starting `bash` call), `job_id` (`j1`, ...),
`command`, `state` (`running`, `exited`, `failed` for a non-zero exit,
`killed` for a signal, `timed_out`, `stopped`, `lost`), and once it ended
`exit_code` (when it exited) and `duration_ms`, plus `output` (its log file
on the harness machine). The end can arrive long after the call's
`tool_result`, even after the turn ended.

A `tool_progress` event reports on a running tool call (today `bash`):
`call_id`, and optionally `timeout_ms` (the call's effective timeout, sent
once when the command starts and repeated with each sample) and `liveness`
(`cpu_ms`, CPU time the command's live processes have used so far; `procs`,
live processes in its group; `active`, how many are running or runnable).
Samples are taken every 3 s while the command runs; a host that cannot be
sampled (the plain-shell remote fallback) sends no `liveness`. The activity
line uses them to tell a busy, quiet command from an idle one.

An `mcp_call` event follows the `tool_call` of a call that goes to an MCP
server: `call_id` (the same as the `tool_call`'s), `server` (the
`[mcp.<name>]` name) and `tool` (the server's own tool name; the
`tool_call` has the name the model used, such as `mcp__github__create_issue`
or `mcp_tools`). The call's `policy_decision` names the tool
`mcp:<server>/<tool>`, and its `tool_result` has the duration and output.
An `mcp_tools_changed` run-level event records a server's tool-list change
during the run: `server`, `added` and `removed` (selected tools, omitted
when empty) and `tools` (how many selected tools are available now).
See [mcp.md](mcp.md).

A `run_started` event has `name`, and optionally `workflow`, `host`,
`title` (a short title, set for workflow runs from their inputs; other runs
are titled after their first `user_message`) and `agent_names` (agent name
→ YAML `display_name`, so every view names agents the same way). Both are
omitted when empty, so older runs still load.

An `error` event has `message`, `recoverable` (whether the run continued)
and, when the error has a way out, an optional `action`:
`{"label": "Manage usage", "url": "https://chatgpt.com/settings/usage"}`
for a ChatGPT plan usage limit. The field is omitted when there is no action,
so runs written before it existed still load. The TUI shows the action as
the first, focused button of the error panel.

Payloads such as model requests, tool arguments and tool output are written
as `{"text": "…"}` or `{"json": …}`. When a payload is larger than 16 KiB it
is moved to the blob store and replaced by
`{"blob": {"sha256": "…", "size": 123, "encoding": "text"}}`. Blobs are named
by their SHA-256, so identical output (for example the same file read twice)
is stored only once.

### Crashes

Events are written by a buffered background writer that flushes after each
batch. If harness is killed mid-write, the last line may be incomplete. When
the run is read back, an incomplete final line is skipped and the run is
flagged as truncated; everything before it loads normally. A damaged line
anywhere else is reported as an error with its line number.

### Schema versions

The header records the schema version. A build of harness reads the versions
it knows how to migrate and refuses anything else (for example a run written
by a newer harness) with a clear "unsupported event schema version" error
rather than guessing.

## Redaction

Tool output can contain secrets, for example when an agent runs `env`. Every
event passes through a redactor before it is written to disk, shown in the UI
or returned to the agent loop, so tool results are redacted before they are
sent back to the model. Log lines go through the same redactor.

The redactor replaces secrets with `[REDACTED]`. It masks:

- every credential loaded from `harness.ini` (`api_key`, `api_key_cmd`,
  `api_key_env` values); values shorter than 4 characters are ignored;
- common credential formats: `sk-…`/`rk-…` API keys (Anthropic, OpenAI,
  OpenRouter), Stripe keys, GitHub tokens (`ghp_…`, `github_pat_…`), AWS
  access key ids (`AKIA…`), Slack tokens (`xox…`), Google API keys (`AIza…`),
  JWTs, `Bearer`/`Basic` authorization values, `UPPER_CASE_TOKEN=value`
  style assignments (as printed by `env`), passwords in URLs
  (`https://user:password@host`) and PEM private key blocks. A private key
  block that is cut off is masked to the end of the text.

Streaming model output is redacted too. To catch a secret split across
chunks, the stream holds back a short tail (at least 128 bytes, or the length
of the longest configured secret) until more text arrives or the call ends,
so live output may lag by a few words.

## Run state

The views fold the event log into a run state: nodes with their status,
parent, `needs` edges, start and finish times, duration, tokens, cost,
retries, model and tool call counts, changed files, pending approvals, last
verify result and host. The same fold is used live and on replay, so a
replayed run looks exactly like it did while it ran.
