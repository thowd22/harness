# Update check

When the interactive TUI (`harness` with no command) starts, it checks
GitHub for a newer harness release. If it finds one, a dialog shows the
version you run, the new version, a link to the release notes and the
command that installs it:

```text
╭ Update available ──────────────────────────────────────────────────╮
│ A new harness release is available                                 │
│                                                                    │
│ Current version  0.4.0                                             │
│ New version      0.5.0                                             │
│                                                                    │
│ Release notes:                                                     │
│ https://github.com/thowd22/harness/releases/tag/v0.5.0             │
│                                                                    │
│ Install it with:                                                   │
│ curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/ │
│ install.sh | bash                                                  │
│                                                                    │
│ [ copy install command ]  [ copy link ]  [ close ]                 │
│ Tab move · Enter select · Esc close                                │
╰────────────────────────────────────────────────────────────────────╯
```

- **Copy.** `[ copy install command ]` has the focus when the dialog
  opens, so Enter copies the command. Tab and the arrow keys move to
  `[ copy link ]` and `[ close ]`. Every button is clickable. Copies use the same
  clipboard as every other copy action (`[ui] clipboard`, OSC 52 by
  default), so they also work over SSH. The release-notes link is an OSC 8
  hyperlink in terminals that support it.
- **When it appears.** The dialog waits until the chat input is empty
  and no turn is running, so it never opens while you type a prompt or
  takes the keys meant for it. If you start typing before the check
  answers, it appears after you send the prompt and the turn ends.
- **Closing.** Esc or `[ close ]` closes the dialog. Other keys, pastes
  and clicks outside it do nothing. A version you closed is not offered again; the
  next newer release is.
- **Over other dialogs.** A sign-in or `/login` dialog that opens later goes
  on top of it and gets the keys until it closes.

## What is sent

One plain `GET https://api.github.com/repos/thowd22/harness/releases`
with harness's user agent (`harness/<version>`). It carries no
credentials, cookies, project names or other data. harness picks the
newest non-draft release by semantic version. Pre-releases (tags such as
`v0.5.0-rc.1`) count only when the build you run is a pre-release
itself.

The check runs in the background after the session has started, so the
first frame never waits for it. When GitHub cannot be reached, answers an
error or rate-limits the request, nothing is shown; the reason is written
to the log (`harness::update` target, see [logging.md](logging.md)).

## How often

The result is cached in `<data_dir>/update-check.json` (see
[data-dir.md](data-dir.md)): when GitHub was last asked, the newest
stable release and the newest release of any kind it found, and the
version you dismissed. GitHub is asked again only once that check is
older than `[general] update_check_interval` (default `24h`). A failed
check counts too, so an offline machine is not asked at every start.
Until the next check, the cached result decides whether the dialog
opens.

## Turning it off

```ini
[general]
update_check = off            ; on (default) | off; also true / false
update_check_interval = 24h   ; at most one check per interval
```

Only the interactive TUI checks. `harness run`, `harness wf run`,
`harness bench`, `harness eval`, `harness ui-demo` and every other
command never make the request, whatever the setting.

The `HARNESS_UPDATE_API` environment variable replaces the API base URL
(`https://api.github.com`). The test suite sets it to a local address,
so tests never contact GitHub.
