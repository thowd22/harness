# Memory

harness keeps a personal, local memory: a git repository of Markdown notes
(`[memory] drive`, default `~/.local/share/harness/memory`).

- **What is saved**: your corrections (as the rule to follow), preferences,
  decisions with their reasons, and gotchas about repos and tools. Not
  saved: session summaries, task state, facts anyone can read in the repo,
  secrets.
- **How**: agents with `memory: read-write` (the default `coder`) save one
  bullet per lesson with the `memory` tool. Each write is a commit and shows
  an "Updated memory" card with **undo**. Each bullet is one line of at most
  400 characters; several bullets for one note go in one write.
- **"Index this codebase into memory"**: the agent saves a compact
  `project/overview` note (at most 10 bullets of up to 1000 characters)
  plus the non-obvious gotchas it found, not a file map, and tells you the
  rest is read from the repo each session ([details](../memory.md)).
- **When it applies**: new memories apply to future sessions. A session
  loads `MEMORY.md` and the current project's index at start; other notes
  are found with memory search.
- **Dreaming**: about once a day, after activity, a consolidation pass
  merges duplicates, prunes stale unused notes and adds lessons from recent
  runs, as one revertable commit ([dreaming.md](../dreaming.md)).
- **`/memory`**: browse files, search, recent writes and dream history with
  diffs; open a note in `$EDITOR`, ask the agent to change or forget it,
  undo, turn memory off, dream now ([memory-ui.md](../memory-ui.md)).

```ini
[memory]
enabled = true                   ; false turns memory off; notes are kept
dream = daily                    ; daily | manual | off
dream_model = haiku
index_max_tokens = 1500
headless_access = false          ; headless runs, evals and automations get no memory
```

Give an agent no memory with `memory: off` in its YAML.
