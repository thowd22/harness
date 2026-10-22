# Memory in the TUI

## Updated-memory card

Every memory write or forget shows an inline card in the chat
(`harness_tui::memory::card::MemoryCard`, built from the run log's `memory`
event): the note, each bullet added (`+`) or removed (`−`), and the line
"Applies to future sessions; this session keeps its current memory." New
memories never change the running session's prompt (the memory index is
frozen per session so the prompt prefix stays cacheable); the next session
loads them.

The card's **[ undo ]** button (Tab to focus, Enter, or click) reverts the
commit (`harness_memory::dream::history::undo`); the revert is itself a
commit, and the card then shows "undone".

## `/memory`

`/memory` opens the browser (`harness_tui::memory::browser::MemoryBrowser`):

| Tab | Shows | Actions |
|---|---|---|
| Files | every note with its title and number of memories, and the selected note's text | open in editor (`$EDITOR`), ask to change, ask to forget |
| Search | memory search results for the query you type | same as Files |
| Recent writes | memory writes, forgets and undos, newest first, with the selected commit's diff | undo |
| Dreams | dreaming passes with their diffs | undo |

"Ask to change" and "ask to forget" put a prompt such as
`Update the memory note [[tools/cargo]]: ` into the input box; finish the
sentence and send it, and the agent edits memory with its memory tool.

Every view also has **memory off/on** (turns memory off for this session;
notes are kept and come back when it is turned on again; the conversation
continues in the same session, only the memory tool comes or goes),
**dream now** (`/memory dream`) and **close**. "Open in editor" suspends the
UI, runs `$VISUAL` or `$EDITOR` (default `vi`) on the note in the drive, and
redraws when the editor exits.

Keys: Left/Right switch tabs, Up/Down select, typing edits the search query
and Enter runs it, Tab/Shift+Tab move between buttons, Enter activates,
Esc closes. Everything is clickable.

## Turning memory off

- `[memory] enabled = false` turns memory off everywhere; the drive and its
  notes are kept.
- `memory: off` in an agent's YAML gives that agent no memory tool (the
  built-in `reviewer` does this).
- `memory: off` at the top of a workflow gives none of its agent steps a
  memory tool (`memory: read` caps them at reading). Without it, agent steps
  get memory as their own `memory:` setting allows.
- Headless runs (`harness run`), evals and automations neither read nor write
  memory unless `[memory] headless_access = true`.
