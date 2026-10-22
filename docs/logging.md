# Logging and crash handling

harness never writes diagnostics to the terminal while the TUI is running.
Instead, all `tracing` output goes to a log file.

## Where logs go

```
<data_dir>/logs/harness.YYYY-MM-DD.log
```

`data_dir` defaults to `~/.local/share/harness` (`[general] data_dir` in
`harness.ini`). Files rotate daily and the newest 7 are kept; older ones are
deleted automatically. Lines are plain text (no colour codes).

## Choosing the log level: `HARNESS_LOG`

`HARNESS_LOG` takes a `tracing` filter. The default is `info`.

```sh
HARNESS_LOG=debug harness                       # everything at debug and above
HARNESS_LOG=warn,harness_agent=trace harness     # quiet, except the agent loop
```

An invalid filter is reported on stderr at startup and logging is disabled for
that run; harness itself still starts.

## Secrets in logs

Every log line passes through the same redaction layer used for run events
before it is written, so API keys and other secrets that end up in a log
message are replaced with a placeholder.

## Panics

If harness panics, it first restores your terminal (leaves raw mode and the
alternate screen, disables mouse capture, shows the cursor), then writes the
panic message and backtrace to the log file at `ERROR` level (target
`harness::panic`), and finally prints the panic to stderr as usual. Set
`RUST_BACKTRACE=1` to include a backtrace.

## For developers

- `harness_events::logging::init(&LogConfig)` installs the global subscriber;
  `build_subscriber` returns it without installing (for tests).
- `LogConfig::with_redactor` takes any `harness_events::logging::LineRedactor`
  (closures `Fn(&str) -> String` work too). The binary should pass the
  redactor from `harness_events::redact` once it exists.
- `harness_tui::terminal::install_panic_hook()` must be called after logging is
  initialised and before the terminal enters raw mode.
