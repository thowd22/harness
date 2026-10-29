# The command palette

**Ctrl+P** opens one fuzzy-searchable list of everything you can do:
built-in commands, workflows, skills, agents, past runs, saved sessions,
remote hosts, MCP prompts and resources, and settings, including the autonomy modes (`ask`, `auto-edit`, `auto`, `yolo`). Each
row shows where it comes from (for example `skill · project`) and its key
binding.

Type to filter, Up/Down to select, Enter to run. Entries that need
arguments ask for them inline; Esc goes back, Esc again closes. Rows are
clickable.

Typing `/` at the start of the message box opens the same fuzzy search
for slash commands only: `/mdl` finds `/model`, `/deploy` finds
`/project:deploy`, and a command whose description matches is listed
below the name matches. Tab or Enter completes the selected row.

Rebind the palette (or any action) in `harness.ini`:

```ini
[keys]
palette = ctrl+k
newline = shift+enter, alt+enter
```

Built-in entries include Toggle mouse, Show keys, Switch view, Interrupt,
Notifications, Timestamps, Thinking, Mode and the copy actions (Copy last
message, Copy last code block, Copy tool output, Copy diff, Select text). Workflows appear as their
slash command, skills as `/name`, agents as their command with their
display name and one-line description, for example
`/coder  Switch to Coder: Default coding agent (built in)`.

MCP prompts appear as `/<server>:<prompt>` with their arguments, and MCP
resources as `Attach MCP resource: <server>/<name>`, which attaches the
resource, marked as untrusted content, to your next message (see
[MCP in the TUI](../mcp.md#in-the-tui)).

Past runs read like `Coder · fix the header parser · 14:32 Oct 23 · ✔`:
the agent (or workflow), a title taken from the first prompt (or the
workflow's inputs), when it started and how it ended. The description says
how it went, for example `Succeeded in 2m14s · $0.12 · 3 files changed`.
Run ids are never shown; if two runs would read the same, each gets a short
`#tag` such as `#7a2b02` after its title, which `harness runs show` accepts
too. On a narrow terminal the title is shortened so the tag, time and status
stay visible. With nothing typed, past runs are listed newest first.

Saved sessions read like `Resume Fix header parser · 14:32 today`, with
the description, project, model, message count and cost below; choosing
one resumes it. The ten most recently active sessions other than the open
one are listed. `/resume` opens the full list ([sessions](../sessions.md)).
