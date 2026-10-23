---
id: doc-7
title: 'Visual Design - Dracula Theme, Indicators, Tool Cards'
type: specification
created_date: '2026-10-22 14:43'
updated_date: '2026-10-23 04:34'
---
# Visual Design: Dracula Theme, Working Indicators, Tool Cards

Look and feel target: dense, informative and calm, in the spirit of pi / oh-my-pi: tool calls as cards, live working indicators, timestamps and token/cost detail everywhere, with the Dracula palette.

## Palette (Dracula) and semantic tokens

Components never use raw colors, only semantic tokens. The default theme maps them to Dracula:

| Dracula | Hex | Semantic tokens |
|---|---|---|
| Background | `#282a36` | `bg` |
| Current Line | `#44475a` | `surface` (cards, selection, focused row), `border.focus` bg |
| Foreground | `#f8f8f2` | `fg`, assistant text |
| Comment | `#6272a4` | `muted` (timestamps, metadata, borders), `reasoning` (italic) |
| Cyan | `#8be9fd` | `running` (spinners, active nodes), `path` / `link` |
| Green | `#50fa7b` | `ok`, diff added |
| Orange | `#ffb86c` | `warn`, `approval` (waiting on user), `stalled` |
| Pink | `#ff79c6` | `user` (user message bar), `keyword` |
| Purple | `#bd93f9` | `accent` (tool names, focus border, palette match), `agent` names |
| Red | `#ff5555` | `error`, diff removed |
| Yellow | `#f1fa8c` | `number` (tokens, cost, durations), `highlight` (search match, slash-completion match) |

Card tints (blended over `bg`): `card.pending` (surface), `card.ok` (green at 8 %), `card.error` (red at 10 %), `card.approval` (orange at 10 %). Diff lines use `ok` and `error` at 15 % for backgrounds with full-strength gutter signs.

Color depth: truecolor when `COLORTERM` is `truecolor` or `24bit`, else nearest-256, else 16-color mapping; `NO_COLOR` or `theme = none` disables color but keeps icons and layout. `[ui] background = theme | terminal` chooses between painting Dracula `bg` or leaving the terminal's own background. Every token can be overridden in `[theme]` in harness.ini.

## Working indicators

Something is always visibly happening, or visibly not.

**Main window activity line** (just above the input while the main agent works):

```
⠹ thinking · turn 4 · 12.3s · ↑12.4k (cache 11.9k) ↓812 · $0.021 · esc to interrupt
⠼ running bash: cargo test parse · 8.1s · streaming 214 lines
⠧ retrying in 4s (429 rate limited, attempt 2/5)
⏸ waiting for approval: git push origin main
⠏ verifying: cargo test --workspace · 31s
⠋ compacting context · 3.2s
```

Phases: sending, waiting for first token, thinking, writing, running tool (name + summary), retrying (reason, countdown), waiting for approval, verifying, compacting, syncing to remote host. Each phase shows its elapsed time; the turn total is kept too.

**Stall detection:** no bytes from the provider or no tool output for `[ui] stall_after` seconds turns the indicator orange: `⚠ no data for 20s (provider stream)`.

**Per agent:** spinner on running map nodes, goal-bar tasks and subagent cards, colored `running`; the status line shows `◐ 3 agents · 1 remote`; optional terminal-title spinner via OSC.

**Animation:** braille spinner at ~80 ms only while something runs and is on screen; animation ticks redraw only the affected cells. `[ui] animations = false` uses a static ◐.

## Timestamps and metadata

- Every user message, assistant turn, tool call and event card has a timestamp, right-aligned in `muted`: `14:32:07`. `[ui] timestamps = time | relative | off`.
- Each assistant turn ends with a muted metadata line: `sonnet · ↑12.4k (cache 96%) ↓812 · reasoning 1.2k · $0.021 · 4.2s · end_turn`.
- Tool cards show duration; turns show wall time; runs show total and critical-path time.

## Tool call cards

One card per call. Header: status icon, tool name (`accent`), concise argument summary, right side duration and timestamp. Body: output preview, collapsed past `[ui] tool_preview_lines`.

```
╭ ✔ edit  src/parse.rs  +3 −1 ─────────────────────────────── 3ms · 14:32:09 ╮
│  40 │   let hdr = buf.split_once(':')                                      │
│  41 │-      .unwrap();                                                     │
│  41 │+      .ok_or(ParseError::MissingColon)?;                             │
╰──────────────────────────────────────────────────────────────────────────────╯
╭ ✗ bash  $ cargo test parse ──────────────────────── exit 101 · 8.1s · 14:32:18 ╮
│ test parse::header_colon ... FAILED                                          │
│ … 212 more lines · Enter to expand · full output saved                       │
╰──────────────────────────────────────────────────────────────────────────────╯
╭ ⏸ approval  bash  $ git push origin main ───────────── outward action · 14:33:02 ╮
│ rule: outward.ask = git push*        [y] once  [s] session  [n] deny         │
╰──────────────────────────────────────────────────────────────────────────────╯
```

- **read:** path, line range, lines returned, truncated badge.
- **write:** path, bytes, `new file` badge, preview.
- **edit:** unified diff with syntax colors and line numbers, +/− counts.
- **bash:** `$ command`, live streaming output while running, exit code badge, timeout/killed badge, spill pointer when truncated.
- **todo / memory / spawn_agent:** compact summaries (task changes, memory note updated, subagent card).
- **errors:** red card with the actionable error message given to the model.
- Parallel calls in the same turn are grouped under one header with a count.
- Cards are clickable and keyboard-focusable: Enter expands or collapses, palette offers copy output and open in node detail.
- **Copy buttons:** the bottom border carries `⧉ copy output` and (for edits and writes with a change) `⧉ copy diff` in `accent`, e.g. `╰─ ⧉ copy output ─ ⧉ copy diff ───╯`. User and assistant headers show a muted `⧉ copy` before the timestamp, and every code block starts with a muted header line (`  rust  ⧉ copy`). Each button is its own focusable element (focused: `surface` background); copying confirms in a success toast. Keyboard selection (`ctrl+t`) highlights the selected lines with the focus style, the cursor line bold and underlined, and shows a `SELECT` bar (`accent` badge, `[copy] [mark] [done]` buttons) in place of the input.

## Subagent cards (chat)

```
╭ ◐ researcher · haiku · ⧉ gpu-box ───────────────────────── 12s · $0.004 ╮
│ ⠼ reading src/net/mod.rs                                                 │
╰──────────────────────────────────────────────────────────────────────────╯
```
Live last action, cost and duration; click or Enter opens the agent in the map or node detail.

## Chat transcript scrolling

The transcript is bottom-anchored: the newest line sits just above the input, a transcript shorter than the pane is padded above so it hugs the input, and older output scrolls away upward. Scrolling up pins the top visible line (entry + line offset), so new output never moves what the user is reading; a `↓ new messages · end` button (`accent` on `surface`, focus style when focused) appears on the bottom transcript row once output arrives below. Clicking it, Enter on it, End (when the input would not use it) or Ctrl+End, scrolling to the end, or the palette's follow entry resume following. A width change keeps the pinned entry on top at the same relative offset; a pinned view that can show the whole tail follows again. Frames lay out only visible entries.

## Status line (footer)

`~/CODE/harness  main │ sonnet · auto-edit │ ctx ▓▓▓▓░░░░ 41% │ ↑182k ↓14k cache 93% │ $0.84 │ verify ✔ │ ◐ 3 agents │ ⧉ gpu-box`

## UI gallery

`harness ui-demo` renders every component (cards, indicators, map, goal bar, palette, approvals) with fake-provider data, for visual review and snapshot tests.
