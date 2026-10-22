# Dreaming: memory consolidation

Without consolidation memory grows noisy and stale. A dreaming pass
(`harness_memory::dream::Dreamer`) tidies the memory drive in one commit:

1. **Merge duplicates.** Bullets with the same text anywhere in the drive are
   merged into one (the copy in `MEMORY.md` wins, then `tools/`, then
   `projects/`); near duplicates within a note are merged too. The survivor
   keeps every source link (`(src: harness://run/a,harness://run/b · date)`).
   Notes left empty are deleted and `[[links]]` to them are re-pointed.
2. **Drop transient details**: task state and session summaries (the same
   rules the memory tool applies to new writes).
3. **Prune stale notes**: a note nobody read or found (usage stats in
   `.harness/usage.jsonl`) within the grace period, and whose bullets are all
   older than it, loses its bullets. `MEMORY.md` is never pruned.
4. **Add missed lessons** from run logs since the last pass, proposed by
   `[memory] dream_model` and screened by the memory write guard (no
   secrets, task state or summaries). The first pass reviews the most recent
   runs, which seeds memory for new users.
5. **Rebuild the index**: a generated block at the end of `MEMORY.md` lists
   every note with its title and number of memories.

**Invariants.** Explicit preferences (bullets such as "prefers", "always",
"never", "do not", "rejected") are never pruned, removed or merged with a
merely similar bullet, and source links are never dropped by a merge. The
pass checks both before it writes anything and aborts if either would break.

**One commit.** The result is a single commit with subject `memory: dream`
whose message lists every change. It appears under Dreams in `/memory`, where
its diff is shown and **undo** reverts it (`dream::history::undo`). Each pass
is also recorded in `.harness/dream-log.jsonl`.

## When it runs

`[memory] dream` decides:

| Value | Behaviour |
|---|---|
| `daily` (default) | a pass runs when at least `interval_hours` passed since the last one and there was activity (a run or memory write) since then; checked at start-up and when idle |
| `manual` | only on `/memory dream` |
| `off` | never |

`/memory dream` runs a pass immediately (unless dreaming is off).
`dream::schedule::due` implements the rule.

## Settings

Settings live in the built-in `dream` workflow, which you can override with
your own `~/.config/harness/workflows/dream.yaml`:

```yaml workflow
name: dream
description: Consolidate long-term memory
command: dream
steps:
  - id: consolidate
    type: dream
    with:
      grace_days: 30        # unused notes older than this are pruned
      max_new_lessons: 5    # lessons added from run logs per pass
      recent_runs: 20       # runs reviewed per pass
      interval_hours: 20    # minimum time between scheduled passes
```

`DreamSettings::from_workflow` reads the `with:` map of the first
`type: dream` step; missing keys keep their defaults and unknown keys are
errors.
