# The command palette

**Ctrl+P** opens one fuzzy-searchable list of everything you can do:
built-in commands, workflows, skills, agents, past runs, remote hosts and
settings, including the autonomy modes (`ask`, `auto-edit`, `auto`). Each
row shows where it comes from (for example `skill · project`) and its key
binding.

Type to filter, Up/Down to select, Enter to run. Entries that need
arguments ask for them inline; Esc goes back, Esc again closes. Rows are
clickable.

Rebind the palette (or any action) in `harness.ini`:

```ini
[keys]
palette = ctrl+k
newline = shift+enter, alt+enter
```

Built-in entries include Toggle mouse, Show keys, Switch view, Interrupt,
Notifications, Timestamps, Thinking, Mode and the copy actions (Copy last
message, Copy last code block, Copy tool output, Copy diff, Select text). Workflows appear as their
slash command, skills as `/name`, agents as "Run agent ...".
