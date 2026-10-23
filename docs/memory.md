# Memory

Harness keeps a long-term memory so a new session does not start from zero.
Memory is a local git repository of Markdown notes, called the *memory
drive*. It belongs to you: browse it, edit it in `$EDITOR`, or use
`git log` and `git revert` on it.

Crate: `harness-memory`. Design: `backlog/docs/doc-4 - Memory-System-Design.md`.

## The drive

The drive lives at `[memory] drive`. By default that is `<data_dir>/memory`,
usually `~/.local/share/harness/memory`. Harness creates it on first use:

```text
memory/
  MEMORY.md                 general preferences and an index of other notes (always loaded)
  projects/<project-key>/   notes for one project, e.g. testing.md, architecture.md
  tools/                    notes about tools, e.g. cargo.md, docker.md
  .harness/                 internal, git-ignored: usage stats, search index, session worktrees, lock
  .gitattributes            *.md merge=union (concurrent additions merge cleanly)
  .gitignore                /.harness/
```

The drive is a git repository on branch `main`. Harness makes every commit
as `harness <harness@localhost>`, with hooks and commit signing turned off,
so your global git config cannot break memory writes.

`.harness/` is a **protected path**. Agent tools (`write`, `edit`, `bash`)
may never modify it. The data dir is protected already. If you point
`[memory] drive` somewhere outside the data dir, the agent wiring adds
`Drive::protected_paths()` to the policy engine's protected roots.

### Project keys

A project's notes live under `projects/<key>/`. The key is `<slug>-<8 hex>`:

- If the project has a git remote (`origin`, or else the first remote), the
  key comes from the remote URL. The URL is normalised first: lower case,
  with no scheme, user, port or `.git` suffix. So
  `git@github.com:Me/App.git` and `https://github.com/me/app` give the same
  key, and every clone of a repository shares its notes.
- If it has no remote, the key comes from the canonical project path, so
  symlinks to the same directory share a key.

### Note format

A note is a list of one-line bullets. The harness adds the source run and
the date to each bullet. Notes link to each other with `[[path]]`, where the
path is relative to the drive and has no `.md`:

```markdown
# Testing (my-app-1a2b3c4d)
- Run `just test-int` for integration tests; plain `cargo test` skips them. (src: harness://run/01J.. · 2026-10-21)
- Prefer table-driven tests; user rejected per-case fns. See [[projects/my-app-1a2b3c4d/style]] (src: harness://run/01J.. · 2026-10-18)
```

## The `memory` tool

An agent gets the tool only when its YAML sets `memory: read` or
`memory: read-write`. With `memory: off` (the default) the tool is not sent
to the model at all.

| Action | Access | Arguments | Effect |
|---|---|---|---|
| `search` | read | `query`, `limit?` (default 5, max 20) | BM25 search; returns notes and their best-matching bullets |
| `read` | read | `path` | returns a whole note |
| `write` | read-write | `path`, `text` or `bullets`, `title?` | appends one bullet (`text`) or up to 10 (`bullets`) to the note in one commit (a new note gets `title`, or one built from its file name) |
| `forget` | read-write | `path`, `text` | removes every bullet that contains `text` (case-insensitive); a note left with no bullets is deleted (`MEMORY.md` is kept) |

Paths can be `MEMORY.md`, `tools/<tool>`, `projects/<key>/<topic>`, or the
shorthand `project/<topic>` for the current project. Writing the same
bullet twice does nothing.

The tool description and schema state the limits up front: each bullet is
one line of at most 400 characters (1000 in `project/overview`, see below),
and the schema sets `maxLength` on `text` and on each `bullets` item and
`maxItems: 10` on `bullets`. Several lessons for the same note go in one
`write` as `bullets`. Each bullet is checked on its own: the valid ones are
saved together in one commit, and the reply lists every rejected one by
number with its reason (`Not saved: bullet 3: the memory is 450
characters; the limit is 400 per bullet. Split it into several
self-contained bullets ...`), so the model resends only those. A write
whose bullets all fail is refused with the same per-bullet list and makes
no commit.

Memory is also turned off when `[memory] enabled = false`, and for headless
runs (automation, evals) unless `[memory] headless_access = true`. See
`tool::effective_access`.

### What gets saved

The tool description and `extract::PROMPT_SECTION` tell the agent to write a
memory in these cases:

- the user corrects it: it saves the rule to follow, not the mistake;
- the user states a preference;
- a decision is made: it saves the decision with its reason;
- it hits a non-obvious gotcha about the repo, the environment or a tool.

Every write goes through the same checks, and is refused with a message the
model can act on if it fails one:

1. **Format.** The bullet is a single line of at most 400 characters
   (1000 in a project's overview note). It is not a heading, has no
   hand-written `(src: …)` suffix, and every `[[link]]` is a valid note
   path. A bullet that is too long is refused with its length, the limit
   and how to split it into several bullets sent in one write.
2. **Secrets.** The text is scanned with the run's redactor: your configured
   credentials plus the built-in key patterns. Anything the redactor would
   mask is refused, and the secret is never echoed back.
3. **What is not saved**, checked with heuristics:
   - task state: PR or issue numbers, ticket keys such as `PROJ-12`,
     statuses ("is blocked", "merged"), TODO/WIP, next steps, commit hashes;
   - session summaries: "in this session…", "today we…", "we implemented…";
   - facts anyone can rediscover from the repo: "function X is defined in
     Y", "main.rs contains…", "the repo has 13 crates".

### "Index this codebase into memory"

Users sometimes ask the agent to index or learn a whole codebase into
memory. A file-by-file or module map is exactly the kind of fact the rules
above refuse: it is easy to rediscover and goes stale with every change.
Harness answers such a request this way, taught by the tool description and
`extract::PROMPT_SECTION`, and enforced by the write checks:

- The agent writes one **overview note**, `project/overview` (stored as
  `projects/<key>/overview.md`): the project's purpose, its architecture in
  a few sentences, how to build and test it, and the conventions that
  matter. Overview bullets may be up to 1000 characters
  (`guard::MAX_OVERVIEW_BULLET_CHARS`), and the note holds at most 10
  bullets (`guard::MAX_OVERVIEW_BULLETS`). A write that would grow it past
  that is refused with a hint to read the note and forget or merge bullets
  first; re-sending bullets it already has is not refused.
- Everything else it found worth keeping goes in as **non-obvious
  gotchas** in ordinary project notes (`project/testing`,
  `project/gotchas`, ...), several per write.
- It **tells the user** that the rest of the code is not stored: it is read
  from the repository again in each session, which stays accurate as the
  code changes.

The overview note is listed in the session index like every other project
note, so later sessions find it with `read project/overview`.

### Commits

Each write or forget is one commit on `main`. The message names the note
and the bullets, and ends with a run reference:

```text
memory: write tools/cargo.md

- Use cargo nextest; plain cargo test times out in CI.

Run: harness://run/<run id>
```

`MemoryTool::run` returns a `MemoryChange` (action, note, bullets, commit).
The agent records it as an `EventKind::Memory` event, and the TUI shows it as
the "Updated memory" card. To undo a change, run `git revert <commit>` in
the drive.

## Retrieval

- **At session start**, `index::SessionIndex::load` reads `MEMORY.md` and
  builds an index of the current project's notes (one
  `- [[link]] Title (N memories)` line per note). The result is capped at
  `[memory] index_max_tokens`, estimated at 4 bytes per token. If it is cut,
  it ends with a note saying more is available through search. It goes into
  the stable prompt prefix.
- **The index is frozen.** It is computed once and never changes during the
  session. Memories written during a session only show up in the next one,
  which keeps the prompt prefix byte-identical and cacheable.
  `SessionIndex::sha256()` is there for cache-stability checks.
- **Search** uses BM25 (k1 = 1.2, b = 0.75) over each note's title and
  bullets. The index is cached in `.harness/search-index.json`. Updates are
  incremental: only notes whose content hash changed are tokenised again.
  It is a small built-in implementation rather than tantivy, because notes
  are small and the index has to stay dependency-light. Embeddings may come
  later.
- **Usage stats.** Every `read`, and every note returned by `search`, is
  appended to `.harness/usage.jsonl` with the note, the run id, a timestamp
  and the kind (`read`, `search_hit` or `index`). Dreaming uses
  `UsageLog::summary()` (per note: reads, search hits, last use, runs) to
  prune notes nobody uses.

## End-of-session extraction

`extract::Extractor` is an optional pass that runs at the end of a session.
It sends a redacted transcript to a cheap model, normally
`[memory] dream_model` (default `haiku`) resolved through the model registry,
together with the memory that is already loaded. The model replies with a
JSON array of `{path, text, reason}` candidates. Each candidate goes through
the same write checks as the tool, and at most 5 are kept. Nothing is
written automatically: show the proposals to the user, then call
`extract::write_proposals(&tool, &accepted)` to save the ones they accept.

## Concurrency

Parallel sessions and subagents can write memory at the same time:

- **One worktree per session.** `sync::SessionCheckout::open(&drive, id)`
  creates `.harness/worktrees/<id>` on branch `session/<id>`. That session
  reads and writes there. Subagents share their parent's checkout, which
  serialises writes internally. `close()` removes the worktree and its
  branch.
- **Stale writes are rejected and retried after a rebase.** A write edits
  the session worktree and commits there. It then takes the drive lock
  (`.harness/drive.lock`) and fast-forwards `main`. If `main` moved because
  another session published first, the publish is rejected as stale. The
  session branch is rebased onto the new `main` while the lock is held, and
  the publish is tried again, up to 8 attempts. `Published { attempts,
  rebases }` reports what happened. A lock older than 2 minutes counts as
  abandoned and is broken.
- **Edits are merged, never overwritten.** Notes use git's `union` merge
  driver. When two sessions add bullets at the same place in a note, both
  bullets are kept and no conflict markers appear. History stays linear,
  with no merge commits. One limit of union merges: if one session forgets
  a bullet while another adds a bullet right next to it, the forgotten
  bullet can come back. Forget it again, or let Dreaming clean it up.

## Configuration

These `[memory]` keys are read (see the harness.ini reference). This crate
adds no new keys.

| Key | Default | Used for |
|---|---|---|
| `enabled` | `true` | `effective_access` |
| `drive` | `<data_dir>/memory` | `Drive::from_config` |
| `index_max_tokens` | `1500` | `SessionIndex::load` cap |
| `headless_access` | `false` | `effective_access` |
| `dream_model` | `haiku` | suggested extraction model |
