# Data directory

harness keeps all of its state under one directory, `data_dir`. Set it with
`[general] data_dir` in harness.ini. The default is `$XDG_DATA_HOME/harness`,
or `~/.local/share/harness` when that variable is unset. The
`HARNESS_DATA_DIR` environment variable, when set and non-empty, overrides
both for one process (a relative path is resolved against the current
directory). `harness eval run` sets it so each eval attempt gets a
throwaway data directory, and tests use it to stay off your real one.

```text
<data_dir>/
  layout-version    layout version number (currently 1)
  runs/             one directory per run: events, artifacts, spill files
  sessions/         saved chat sessions, for resume
  checkpoints/      file snapshots used by /undo
  trust/            project trust store (see docs/trust.md)
  memory/           memory drive, unless [memory] drive points somewhere else
  remote-cache/     remote helper binaries and per-host sync state
  logs/             daily log files (see docs/logging.md)
  model-lists/      cached live model lists for the model catalog (see docs/models.md)
  update-check.json last TUI update check and the dismissed version (see docs/update-check.md)
  auth/             Sign in with ChatGPT token store, created on first sign-in (see docs/providers-openai.md)
    mcp/            OAuth tokens for remote MCP servers, one 0600 file per server (see docs/mcp.md)
```

harness creates any missing directories at startup. On Unix they get mode
`0700`.

## Versioning and upgrades

`layout-version` holds a number. At startup harness compares it with the
version it supports:

- **Older or missing:** harness runs its migration steps one at a time and
  rewrites the file after each step, so an interrupted upgrade picks up where
  it stopped. A directory with no version file counts as version 0. That is
  what earlier builds created, and it may hold only `logs/`. Upgrading it
  creates the standard directories and leaves existing files alone.
- **Newer:** harness refuses to start and asks you to upgrade, so an older
  build never damages newer data.

## Protected paths

Agent tools cannot read or write any of these:

- the data directory
- each internal directory listed above, including a memory drive stored
  elsewhere
- the version file
- `auth/`, the Sign in with ChatGPT token store and the MCP OAuth tokens (`auth/mcp/`)
- `harness.ini`

`DataDir::protected_paths` returns this list, and the policy engine adds it to
the user's `[policy] paths.protected` patterns.
