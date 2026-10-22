# Sessions

A session is one conversation with an agent in one project. It is saved
continuously: the event log is written as the agent works, and the session
metadata (`<data_dir>/sessions/<id>/session.json`) is rewritten atomically
after every agent turn.

```sh
harness sessions
# 0193f2c4-7a1b  2026-10-22 14:05    $0.0412  /home/me/proj  fix the parser bug
harness run --resume last "now add a test for it"
harness run --resume 0193f2c4 "..."
```

`harness sessions` lists sessions newest first with id, time, cost, project
and title (the first line of the first prompt). Costs that are partly unknown
are shown with a `+`.

## Resume

Resuming rebuilds the provider conversation from the latest run's event log,
exactly as it was sent: system prompt, messages, signed reasoning blocks and
provider-opaque blocks (encrypted reasoning, redacted thinking) included.
Tool results recorded after the last model call are restored too, and a tool
call that never got a result (the harness was killed mid-call) gets an error
result so the conversation is valid again.

A resumed session continues in a new run of the same session, so a log that
was cut off by a crash is never appended to. If the last event was torn, a
warning says so.

## Files changed on disk

When a session is saved, the hashes of every project file the agent read,
wrote or edited are stored. On resume, each file is checked again and a
warning is shown for each one that was modified, deleted or created since:

```text
harness: warning: src/parser.rs was modified on disk since this session was saved; the agent's view of it may be stale
```
