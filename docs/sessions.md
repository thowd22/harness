# Sessions

A session is one conversation with an agent in one project. It is saved
continuously: the event log is written as the agent works, and the session
metadata (`<data_dir>/sessions/<id>/session.json`) is rewritten atomically
after every agent turn.

## Names, descriptions and last activity

Every session has:

- a **short name** of a few words, taken from the first prompt. A short
  prompt stays whole (`hi there` → `Hi there`); otherwise openers such as
  `please` or `can you` and articles are dropped, at most five words are
  kept, and the name is cut on a word without ending on a preposition,
  article or conjunction (`Please fix the header parser so it keeps
  colons` → `Fix header parser`; `Fix flaky login test in the auth module`
  → `Fix flaky login test`). `/rename <name>` sets your own name, which is
  then kept and confirmed with `Session renamed to <name>`; `/rename`
  alone shows the current one.
- a **one-line description**, updated after every turn: the first request,
  the latest one when there were several, and the files the agent wrote or
  edited (`fix the header parser → add tests · edited src/parse.rs,
  tests/parse.rs`).
- the **time of the last message** and the **message count** (prompts and
  replies). Only what you typed counts as a request: tool results, the
  verifier's fix feedback and compaction handoffs are neither shown as the
  latest request nor counted.

Names and descriptions are derived locally, so they cost no model call.
To have a cheap model write them instead, set
`[general] session_summary_model` in `harness.ini`:

```ini
[general]
session_summary_model = haiku   ; default: off
```

After each turn the model gets a short, redacted transcript and replies
with a name and a one-sentence description. The call runs in the
background, so it never delays the turn: the list shows the new text once
the reply arrives. It is skipped after a cancelled turn and while a budget
is exhausted, and it is cancelled when the next turn starts, on `/rename`,
and when the session closes (`/new`, `/resume`, quitting); `harness run`
waits for it before exiting unless interrupted. A name you chose with
`/rename` is never replaced. If the call fails, is cancelled or times out
(30 s), the locally derived text stays.

The summary call is recorded in the run log like any other model call: a
`session-summary` child node of the turn with its request, usage and cost.
Its cost counts towards the session's cost, `/cost` and the `[budget]`
limits.

## Listing sessions

`harness sessions` takes no arguments; `harness sessions --help` (or `-h`)
prints its usage.

```sh
harness sessions
# Fix header parser         14:32 today        /home/me/proj  anthropic/claude-sonnet-5-5   12 msgs    $0.0412  #7a2b02  fix the header parser → add tests · edited src/parse.rs
# Release notes             yesterday 09:10    /home/me/proj  anthropic/claude-sonnet-5-5    4 msgs    $0.0100  #c41d9e  draft the release notes
harness run --resume "fix header parser" "now add a test for it"
harness run --resume last "..."
harness run --resume '#7a2b02' "..."
```

Sessions are listed most recently active first, one per line: name, last
activity, project, model, message count, cost, short tag and description.
Costs that are partly unknown are shown with a `+`. Sessions that never got
a prompt (the TUI saves one when it starts) are left out.

The last activity is relative for recent days, in your local time zone:
`14:32 today`, `yesterday 09:10`, then `Oct 21 16:05`, and
`Oct 21 2025 16:05` for other years. Days are calendar days, so `23:59`
and `00:01` are on different days.

`--resume` (for `harness` and `harness run`) and `/resume` accept, in this
order: `last` (the most recently active session with a prompt), a full id,
a name (ignoring case), an id prefix, a short `#tag`, and a unique name
prefix. When several sessions share a name, use the tag.

## The `/resume` picker

`/resume` without an argument opens a dialog over every saved session.
Each session takes two rows: its name with the last activity, project,
model, message count and cost, then its description. The open session is
marked with `●`.

- Type to filter: fuzzy matching over the name, description, project and
  tag. With nothing typed the list stays newest first.
- Up / Down / Tab, PgUp / PgDn and Home / End move; Enter resumes; Esc or a
  click outside closes.
- A click on either row of a session resumes it, and the mouse wheel moves
  the selection.

The palette lists the ten most recently active sessions too.

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
