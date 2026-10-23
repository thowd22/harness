# Terminal UI

The `harness` TUI is built on ratatui and crossterm. This page covers the
application loop, colours, key bindings and mouse/keyboard interaction. The
chat, map and other views are documented separately as they land.

## Views and screens

The app (`harness_tui::app::App`) composes the views:

- **Chat** (`ChatView`) and **Map** (`MapView`), switched with Tab or the
  footer tabs. The **goal bar** (`GoalBar`) sits on the right of both when
  the run has tasks; on narrow terminals it collapses into a status-line
  indicator. Click a task, or use the palette's "Focus goal bar", to reach
  it from the keyboard.
- **Screens** on top of either view: node detail (Enter or a click on a map
  node or subagent card), the diff review (`/diff`, or a node's "review
  diff"), the request inspector (a node's "inspect"), command panels
  (`/cost`, `/config`, `/context`, `/remote`, `/help`, ...) and the memory
  browser (`/memory`). Esc backs out of each.
- The **model picker** (`/model` without an argument, or the palette's
  "Switch model") opens over everything. See [models.md](models.md#the-model-picker-model).
- The **palette** (Ctrl+P) opens over every view and screen. It lists the
  built-in actions, every slash command with its argument help, workflows,
  skills, agents, recent runs and remote hosts, and asks for a command's
  missing arguments.

Clicks go to the component under the pointer; keys go to the open overlay,
then the screen, then the view.

## The core side

`harness` (the binary's `tui` module) runs the session core next to the
UI. It runs one agent turn at a time on the session's conversation (typed
messages are queued while a turn runs; Ctrl+C interrupts), carries out slash
commands and palette entries, and answers with the run's events and
`CoreEvent`s: approval prompts (tool calls, workflow gates and budget
overruns all appear in the chat's approval queue), panels, palette entries,
diffs, hygiene reports, artifacts, memory browser data and map badges. Every
turn is preceded by a shadow-git checkpoint, so `/undo` and the map's
"restore checkpoint" (`r` on a node) can go back.

## Event loop and terminal handling

- Everything the UI reacts to arrives on one channel: key and mouse input,
  terminal resizes, live run events from the core, commands from the palette,
  and signals. The UI answers the core through a separate unbounded channel,
  so the agent never waits on the screen.
- The screen is redrawn only when something visible changed. Bursts of events
  (streaming tokens, many tool events) are folded into one frame.
- When nothing is running the UI sleeps on its channel and uses no CPU.
  Spinners tick every 80 ms only while a run or agent is active and
  `[ui] animations = true`; with `animations = false` a static `◐` is shown
  and no timer runs.
- Resizing the terminal redraws at the new size.
- **Ctrl+Z** suspends harness to your shell; `fg` resumes it with a full
  redraw. Ctrl+Z is fixed and cannot be bound to another action.
- SIGTERM and SIGHUP quit cleanly.
- The terminal (raw mode, alternate screen, mouse capture, bracketed paste) is
  always restored: on normal exit, on errors, and on panics (the panic is
  written to the log file, then printed on a sane terminal).
- Startup to the first interactive frame is checked by a test to stay under
  150 ms.

## Colours and themes

Components never use raw colours; they use semantic tokens. The default theme
is [Dracula](https://draculatheme.com):

| token | default | used for |
|---|---|---|
| `bg` | `#282a36` | background |
| `surface` | `#44475a` | cards, selection, focused element |
| `fg` | `#f8f8f2` | text |
| `muted` | `#6272a4` | timestamps, metadata |
| `reasoning` | = `muted`, italic | model reasoning |
| `running` | `#8be9fd` | spinners, active agents |
| `path`, `link` | = `running` | file paths, links |
| `ok` | `#50fa7b` | success |
| `warn` | `#ffb86c` | warnings |
| `approval`, `stalled` | = `warn` | waiting on you, stalled streams |
| `user` | `#ff79c6` | your messages |
| `keyword` | = `user` | syntax keywords |
| `accent` | `#bd93f9` | tool names, focus, palette matches |
| `agent` | = `accent` | agent names |
| `error` | `#ff5555` | errors |
| `number` | `#f1fa8c` | tokens, cost, durations |
| `highlight` | = `number` | search matches |
| `border` | = `muted` | borders |
| `border.focus` | = `accent` | focused border |
| `card.pending` | = `surface` | running tool card |
| `card.ok` | `ok` at 8 % over `bg` | finished card |
| `card.error` | `error` at 10 % over `bg` | failed card |
| `card.approval` | `approval` at 10 % over `bg` | card waiting for approval |
| `diff.added`, `diff.removed` | = `ok`, `error` | diff text and gutter signs |
| `diff.added.bg`, `diff.removed.bg` | `ok` / `error` at 15 % over `bg` | diff line backgrounds |

Override any token in `harness.ini`:

```ini
[theme]
accent = #ff79c6
warn = #ffaa00     ; approval, stalled and card.approval follow automatically
```

Tokens shown as "= other" or as a tint follow their base token unless you set
them too. A value that is not `#rrggbb` and an unknown token name are load
errors that name the file and line.

Colour depth is detected from the environment:

1. `NO_COLOR` set (non-empty): no colour.
2. `COLORTERM=truecolor` or `24bit`: 24-bit colour.
3. `TERM` containing `256color`: nearest colours from the 256-colour palette.
4. otherwise the 16 basic colours, chosen so that ok, warn, error, running,
   accent and user stay distinct.

`[ui] theme = none` also disables colour. Without colour, icons, layout, bold
and italic remain, and focus is shown in reverse video.

`[ui] background = theme` paints the Dracula background; `terminal` keeps your
terminal's own background.

## Keys

Only a few actions have keys; everything else is in the command palette.

| action | default | |
|---|---|---|
| `send` | `enter` | send the message |
| `newline` | `shift+enter`, `alt+enter` | new line in the input |
| `cancel` | `esc` | close an overlay, step back, or cancel |
| `switch_view` | `tab` | switch between chat and map |
| `palette` | `ctrl+p` | command palette |
| `interrupt` | `ctrl+c` | interrupt the agent |
| `quit` | `ctrl+q` | quit |
| `copy` | `ctrl+y` | copy the focused message, code block, output, diff or log entry |
| `select` | `ctrl+t` | select transcript lines with the keyboard |

Rebind them in `[keys]`; a comma-separated list gives several chords:

```ini
[keys]
palette = ctrl+k, f1
quit = ctrl+x
```

An entry replaces that action's defaults. Chords are written as modifiers
(`ctrl`, `alt`, `shift`, `super`) and a key joined with `+`: a character, `f1`
to `f24`, `enter`, `esc`, `tab`, `backspace`, `delete`, `insert`, `home`,
`end`, `pgup`, `pgdn`, `up`, `down`, `left`, `right`, `space`. Binding one
chord to two actions, or to the fixed `ctrl+z`, is reported at load with the
file and line.

The key help overlay lists the active bindings. Open it with the **keys**
button at the bottom right (click it, or Shift+Tab to it and press Enter) or
from the palette. Esc, the **[ Close ]** button or a click outside closes it.

## Mouse and keyboard interaction

Every visible interactive element can be used with the mouse and the
keyboard:

- **Click** activates the element under the pointer (the topmost one when
  elements overlap). While an overlay is open, clicks outside it close it.
- **Mouse wheel** scrolls the pane under the pointer.
- **Shift+Tab** and the **arrow keys** move focus between elements; **Tab**
  does too inside overlays and when an element other than the input has focus
  (in the input, Tab is `switch_view`). **Enter** activates the focused
  element, **Esc** backs out (closes the overlay or returns focus to the
  input).
- **PgUp / PgDn** scroll the focused pane by a page.
- **End** (or **Ctrl+End**) in the chat jumps back to the latest message
  after scrolling up; see [tui-views.md](tui-views.md#scrolling).
- **/resume** without an argument opens a clickable, fuzzy-searchable
  picker of saved sessions by name and last activity, and **/rename NAME**
  renames the open session; see [sessions.md](sessions.md).

`[ui] mouse = false` starts without mouse capture so the terminal's native
text selection works; the footer then shows `mouse off`. The palette's
"toggle mouse" command switches capture at runtime.

## Copy and paste

Mouse capture turns off the terminal's own text selection, so harness copies
by itself (or set `[ui] mouse = false` to keep native selection).

**What can be copied, and how.** Every target is available by click,
keyboard and palette:

| target | click | keyboard | palette |
|---|---|---|---|
| a message (yours or the assistant's, Markdown as written) | `⧉ copy` in its header | focus the button and Enter, or `ctrl+y` (with nothing focused: the last assistant message) | Copy last message, Copy focused |
| a code block (without fences) | `⧉ copy` on the block's first line | focus it, Enter or `ctrl+y` | Copy last code block |
| a tool call's output | `⧉ copy output` on the card's bottom border | focus it, Enter or `ctrl+y` | Copy tool output (focused card, else the latest); Copy tool output with line numbers |
| a file change (unified diff) | `⧉ copy diff` on the card, `[⧉ copy diff]` in `/diff` | `ctrl+y` on a focused edit card; `y` or `ctrl+y` in `/diff` | Copy diff |
| a node detail log entry | `[⧉ copy entry]` in the header | `y` or `ctrl+y` on the focused entry | Copy entry |
| any range of transcript lines | a click moves the selection cursor | `ctrl+t`, then the keys below | Select text |

**Keyboard selection** (`ctrl+t`): a bar replaces the input and a cursor
line appears on the bottom visible line. Up/Down (or `k`/`j`), PgUp/PgDn
and Home/End (or `g`/`G`) move it; Space or `v` sets the mark; `y` or
Enter copies from the mark to the cursor and leaves; Esc leaves. The bar's
**[copy]**, **[mark]** and **[done]** buttons are clickable and reachable
with Tab.

**What gets copied** is plain text: no ANSI codes or control characters, no
card borders, gutters or copy buttons, and no `read` line numbers unless
you pick "with line numbers". Secrets are masked by the same redactor as
logs and events. A toast says what was copied, how many lines and how
(`OSC 52`, `wl-copy`, ...), also over the map and other screens.

**Where it goes** is `[ui] clipboard`:

- `osc52` (default): the OSC 52 escape sequence. The terminal sets its
  clipboard, so it works over SSH. Inside tmux (`$TMUX` set) the sequence
  is wrapped for passthrough; enable it with `set -g allow-passthrough on`
  (or use `set -g set-clipboard on`). Most terminals support OSC 52; some
  need it enabled and some cap the size.
- `system`: `wl-copy` (Wayland), `xclip` or `xsel` (X11) or `pbcopy`
  (macOS), with a scrubbed environment. If none is found or it fails, the
  copy falls back to OSC 52 and the toast says why.
- `auto`: the system tool when one is usable here (a display, the tool, and
  not an SSH session), else OSC 52.

**Pasting** uses bracketed paste: text arrives exactly (line breaks, tabs,
Unicode, any length) and never acts as keys, so a paste can never send a
message, answer an approval or open the palette. It works with mouse
capture on or off. A paste goes to the palette's filter or argument prompt
when the palette is open (as one line), to the memory browser's search box,
and otherwise to the input, from the chat or the map; pasting while an
approval waits fills the input and leaves the approval open. Large pastes
collapse to a placeholder in the input (see [tui-views.md](tui-views.md#input))
and are always sent in full.
