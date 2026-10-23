---
id: doc-4
title: Memory System Design
type: specification
created_date: '2026-10-22 14:26'
updated_date: '2026-10-23 16:23'
---
# Memory System (Devin-style, local)

Modeled on Devin Memory + Dreaming (https://docs.devin.ai/product-guides/memory), run entirely on the local machine.

## Memory drive

A git repository of Markdown at `[memory] drive` (default `~/.local/share/harness/memory`). It is personal to the local user.

```
memory/
  MEMORY.md                 # general preferences + index of other notes (always loaded, capped)
  projects/<project-key>/   # project-key = slug + hash of git remote URL (or canonical path)
    testing.md
    architecture.md
  tools/
    cargo.md
    docker.md
  .harness/                 # internal (protected path): usage stats, dream log, search index
```

Note format — one-line bullets with inline metadata, `[[path]]` links between notes:

```markdown
# Testing (my-app)
- Run `just test-int` for integration tests; plain `cargo test` skips them. (src: harness://run/01J.. · 2026-10-21)
- Prefer table-driven tests; user rejected per-case fns. See [[projects/my-app/style]] (src: harness://run/01J.. · 2026-10-18)
```

## What is saved / not saved

Saved: work preferences, corrections with the rule to follow, decisions + reasons, gotchas about repos/environments/tools.
Not saved: session summaries, task state (PR numbers, statuses), facts trivially rediscoverable from the repo, secrets/credentials (writes pass a secret scanner and are rejected on match).

## Write path

- Agents with `memory: read-write` get a `memory` tool: `search`, `read`, `write` (note + bullet), `forget`. A `write` carries one bullet (`text`) or up to 10 for the same note (`bullets`), each validated on its own and saved in one commit; the description and schema state the limits (400 characters per bullet, 1000 in `project/overview`), and rejections name the limit and how to split.
- "Index this codebase into memory" is answered with one compact `project/overview` note (at most 10 bullets) plus non-obvious gotchas, not a file map; the agent tells the user the rest is re-read from the repo (TASK-155, see docs/memory.md).
- Triggers taught in the prompt: user correction, explicit preference, reusable discovery.
- Optional end-of-session extraction pass (cheap model) proposes missed lessons from the run log.
- Each write: validate format → secret scan → edit note → commit (message references run id) → merge with concurrent sessions → "Updated memory" card in the TUI (with undo).
- New memories apply to **future** sessions, not the current one (keeps the prompt prefix stable and cacheable).
- Concurrency: each session uses its own git worktree of the drive; stale writes are rejected and retried after rebase; conflicting edits are merged, never overwritten. Implementation: worktrees at `.harness/worktrees/<session>` on `session/<session>`; publish = fast-forward `main` under `.harness/drive.lock`; Markdown uses git's `union` merge driver (`.gitattributes`). See docs/memory.md.

## Read path

- At session start `MEMORY.md` + the current project's index section is loaded into the stable prefix (≤ `index_max_tokens`).
- Other notes are retrieved on demand via `memory search` (built-in BM25 over note bullets with an incremental, hash-keyed index cached in `.harness/search-index.json`; tantivy was dropped as unnecessary for small notes; embeddings optional later) and `memory read`.
- Every read is recorded (note, run, timestamp) in usage stats — used by Dreaming to prune.
- Headless/automation/eval runs neither read nor write memory unless `[memory] headless_access = true`.

## Dreaming (consolidation)

A built-in, user-overridable workflow (`dream.yaml`) that runs about once a day when there was activity (on start or idle), or on demand via `/memory dream`:

1. Merge overlapping/duplicate notes.
2. Remove transient details and stale notes that no session has used (per usage stats, with grace period).
3. Add useful lessons from recent run logs that weren't captured.
4. Reorganize notes and the `MEMORY.md` index.

Invariants: source links and explicit user preferences are preserved; the result is a single commit, viewable as a diff in `/memory` and revertable. The first dream seeds memory from recent runs.

## Controls

- `/memory` browser: files, search, recent writes, dream history + diffs, undo/revert.
- Edit: ask the agent ("forget X", "update Y") or open the note in `$EDITOR`.
- Toggle: `[memory] enabled = false` (notes kept), per agent/workflow `memory: off`.
- Memory vs Skills: memory is learned automatically and personal; skills are deliberate, authored procedures.
