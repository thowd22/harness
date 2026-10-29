# Policy, approvals and sandbox

`harness-policy` decides, for every tool call, whether it may run (`allow`),
needs your approval (`ask`) or is refused (`deny`). It also runs bash inside
an OS sandbox and labels tool output before the model sees it.

Principle: **the repo can ask; only you can grant.** Rules that loosen policy
live only in your `harness.ini`. Agent and workflow YAML can only add
restrictions.

## Precedence

Each tool call is checked against these layers, in order. The first layer
that decides wins:

1. **Built-in hard denies.** Protected paths (below). Nothing can override them.
2. **harness.ini `[policy]`.** `bash.deny`, `paths.protected`, the outward
   guard (`outward.ask`), `bash.ask`, then `bash.allow`.
3. **Agent / workflow YAML `policy:`** (tighten only). Extra `bash.deny`,
   `bash.ask`, `outward.ask` and `paths.protected` rules. Deny and ask rules
   from YAML are checked together with the ini ones, so YAML can turn an
   ini `allow` into `ask` or `deny`, but never the other way round.
4. **Session approvals.** Patterns you approved "for this session". They
   turn an `ask` into `allow`. They never override a `deny`.

When no rule decides, the autonomy mode does:

| Action | `ask` | `auto-edit` (default) | `auto` | `yolo` |
|---|---|---|---|---|
| read a file (`read`, `grep`, `glob`) | allow | allow | allow | allow |
| write / edit a file | ask | allow | allow | allow |
| bash command | ask | ask | allow | allow |
| other tools | allow | allow | allow | allow |

`yolo` goes further than `auto`: nothing ever asks. Whatever would have
asked in `auto` (`bash.ask` and MCP rules, the outward-action guard, the
staging guard, edits outside an agent's scope) runs without a prompt, and
the recorded rule says so (`mode yolo`, naming the rule that would have
asked). Deny rules, protected paths and the bash sandbox still apply:
they refuse rather than ask. Choosing `yolo` shows a warning, and the
status line shows the mode in the error colour.

Every decision is recorded with the rule that produced it: the layer, where
the rule lives (for example `harness.ini [policy] bash.deny`), the pattern
and a one-line reason that is also shown to the model.

## Protected paths

Built-in, denied for **read and write**, and for paths that appear in bash
commands:

- your `harness.ini`
- the data dir (`~/.local/share/harness`): trust store, runs, sessions,
  checkpoints, memory internals, logs
- any `.git/hooks/` directory

Also built-in: **writes** to a project's `.harness/` directory are denied
(reading it is fine).

Add your own with `[policy] paths.protected`, a comma-separated list of globs:

```ini
[policy]
paths.protected = ~/.ssh/**, **/.env*, ~/.aws/**, secrets/**
```

- `~/` is your home directory. Patterns starting with `/` are absolute.
  Patterns starting with `**` match anywhere. Other patterns are relative
  to the workspace root.
- `*` does not cross `/`; `**` does. `dir/**` also protects `dir` itself.
- They apply to the `read`, `write` and `edit` tools, to the `path` of
  `grep` and `glob` (whose results also leave protected files out), and to every bash
  argument, `--opt=value` value, `VAR=value` prefix and redirect target.
  `~`, `$HOME`, `${HOME}` and `$PWD` are expanded, wildcards (`~/.ss*/id_*`)
  are expanded against the filesystem, and symlinks and `..` are resolved,
  so `cat link/../harness.ini` is caught too.

## How bash commands are checked

Matching a pattern against the raw command string is easy to bypass
(`ls && git push`). The policy parser splits a command line into the simple
commands it runs and checks **each one**; the strictest result wins.

It understands:

- lists and pipelines: `;` `&&` `||` `&` `|` `|&`, newlines
- subshells `( )`, groups `{ }`, `if`/`while`/`until`/`for` bodies, `!`, `[[ ]]`
- command substitution `$( )` and backticks, process substitution `<( )` `>( )`
- quoting and escapes: `'..'`, `".."`, `\x`, `$'..'` (with `\x67` style
  escapes), `$".."`, so `g''it push`, `\git push` and `$'\x67it' push` are all `git push`
- brace expansion: `gi{t,} push` is `git gi push`, `{p..p}ush` is `push`
- `VAR=value` prefixes, redirects (`>`, `>>`, `2>&1`, `&>`, `<`, `<<<`) and
  here-documents
- commands that run other commands: `env`, `sudo`, `doas`, `command`,
  `builtin`, `exec`, `nohup`, `time`, `nice`, `ionice`, `timeout`,
  `stdbuf`, `setsid`, `unbuffer`, `xargs`, `watch`, `bash/sh/zsh/dash/ksh -c`,
  `eval` and `find -exec`. The wrapper and the command it runs are both checked.
- `/usr/bin/git` is also checked as `git`.

**Anything else asks.** If the parser cannot tell what a command runs
(function definitions, `case`, `(( ))`, `$CMD args`, `bash -c "$X"`,
unterminated quotes, command substitution inside an unquoted here-document,
very deep nesting, ...), the decision is `ask` (deny rules are still matched
against the raw text). Policy never guesses `allow`.

Patterns are globs over the parsed command: `*` matches anything (spaces and
`/` included), `?` one character. Patterns see each command with its words
joined by single spaces, after quote removal.

`bash.allow` only applies to commands whose words are all known before they
run. `cat $FILE` or `cargo test $(cat args)` are never allowed by an allow
rule; they fall back to the mode (ask, or allow in `auto`).

### Parser spike

| Candidate | Notes | Verdict |
|---|---|---|
| `brush-parser` 0.4 | Full bash grammar (peg), maintained. Large AST; words inside `$( )` need a second parse; pulls `peg` and `cached`. | Good, but heavier than needed |
| `conch-parser` 0.1 | POSIX AST, unmaintained since 2017, no bash-isms (`$'..'`, `[[`, `<( )`). | Rejected |
| `yash-syntax` | POSIX shell, async, built for an interpreter. | Rejected (wrong shape) |
| `tree-sitter-bash` | Robust, error-tolerant, C dependency and build script; error recovery is the wrong default for security (it "succeeds" on broken input). | Rejected |
| `shlex` | Tokeniser only: no operators, substitutions or redirects. | Rejected |
| Hand-written (chosen) | ~600 lines; only the structure policy needs; **fails closed** on anything unknown; no new dependencies. | **Chosen** |

The deciding factor is failure mode: a security parser must reject what it
does not fully understand. Full grammars accept constructs (functions, `case`,
arithmetic) whose effect policy cannot evaluate anyway, so we would have had
to detect and reject them on top of the library. If the supported subset ever
needs to grow a lot, `brush-parser` is the fallback.

## Outward actions

Actions other people can see (pushing, publishing, opening PRs) **ask in
every mode, including `auto`.** Built-in patterns include `git push*`,
`git send-email*`, `gh pr create*`, `gh pr merge*`, `gh release create*`,
`gh api*`, `glab mr create*`, `cargo publish*`, `npm publish*`,
`yarn publish*`, `pnpm publish*`, `twine upload*`, `poetry publish*`,
`gem push*`, `docker push*`, `helm push*`, `terraform apply*` and
`kubectl apply*`. Add your own with `[policy] outward.ask`.

Aliased spellings are caught: `git -C repo -c k=v --no-pager push`,
`/usr/bin/git push`, `gh pr new`, `gh -R o/r pr new`, `npm --registry r publish`,
`cargo +nightly publish`, `docker image push`, plus every wrapper and chain
above (`sudo git push`, `bash -c 'git push'`, `ls | xargs git push`,
`echo $(git push)`).

To let an outward action through without asking, name it **explicitly** in
`bash.allow`: an allow pattern counts only if its fixed prefix (the text
before the first wildcard) is itself an outward command.

```ini
[policy]
bash.allow = git push origin feature/*   ; explicit: pushes to feature branches run
; bash.allow = git *                     ; broad: does NOT bypass the outward guard
```

Deny rules still win over an explicit allow.

### MCP tools

Calls to MCP server tools are decided as `mcp:<server>/<tool>` by
`[policy] mcp.<server>.deny`, `.ask` and `.allow` (tool-name wildcards, in
that order), then by the server's `readOnlyHint` annotations when
`mcp.<server>.trust_annotations = true`. Any other MCP tool is an outward
action: it asks in every mode, including `auto`. See
[mcp.md](mcp.md#permissions).

## Autonomy modes and approvals

The mode comes from `[general] mode` (`ask`, `auto-edit`, `auto`, `yolo`).
Shift+Tab in the chat input steps through them (ask → auto-edit → auto →
yolo → ask), and `/mode` and the command palette (`Mode: ask`,
`Mode: auto-edit`, `Mode: auto`, `Mode: yolo`) set one, for the session. A
per-agent mode can only make it stricter: the effective mode is the
stricter of the session mode and the agent's.

When calls need approval you can answer each one:

- **Once**: this call only.
- **This session**: every command matching a pattern (the prompt suggests
  one, such as `cargo test*`) or, for edits, every path matching a glob,
  until the session ends.
- **Always**: this session, plus a proposal to add the pattern to
  `[policy] bash.allow` in harness.ini. Nothing is written until you confirm
  the proposal. The file keeps its comments, layout and `0600` permissions.
- **Deny**, optionally with a reason that is passed to the model.

Tool calls the model makes in parallel are batched into **one prompt**.
Unanswered requests are denied. Each request and answer is recorded as an
`approval_requested` / `approval_resolved` event on the agent's node, so the
agent map shows the node as waiting for approval. The number of prompts per
run is recorded (`ApprovalMetrics::prompts`, and derivable from the event log).

## Agent and workflow YAML

```yaml
policy:                       # tighten-only
  bash.deny: ["git push*"]
  bash.ask: npm *
  outward.ask: [make release*]
  paths.protected: secrets/**
```

Allowed keys: `bash.deny`, `bash.ask`, `outward.ask`, `paths.protected`.
Keys that would loosen policy (`bash.allow`, `outward.allow`, `paths.allow`,
`mode`, `sandbox.*`) are rejected when the file loads, with an explanation of
where the setting belongs in harness.ini.

## Sandbox

Policy can be wrong, so bash also runs in an OS sandbox (Linux):

```ini
[sandbox]
enabled = true      ; Landlock: writes only in the workspace and temp dirs
network = allow     ; allow | deny
```

- **Filesystem** (`enabled = true`): Landlock allows writes only beneath the
  workspace, `$TMPDIR`, `/tmp`, `/var/tmp` and `/dev/null`, `/dev/zero`,
  `/dev/full`, `/dev/shm`, plus the terminals `/dev/tty`, `/dev/ptmx`,
  `/dev/pts` and the terminal the command was started on. Reads are not
  restricted (protected paths are enforced by policy). Writing to
  `/dev/stdout` or `/dev/stderr` by path is blocked by Landlock; use `>&2`.
- **Device ioctls** (kernels with Landlock ABI 5, Linux 6.10+): only the
  terminals above accept ioctls, so terminal-size probes
  (`ioctl(TIOCGWINSZ)`, `stty size`, `tput cols`), `tcgetattr` and pty-based
  TUI tests work, while ioctls on any other device fail with
  `Permission denied`. On older kernels Landlock cannot restrict ioctls, so
  none are blocked; writes are still restricted.
- **Denials are explained**: when a sandboxed command fails with
  `Permission denied` or `Operation not permitted` that a writable path does
  not explain, its output ends with one `harness sandbox:` line saying the
  sandbox may be the cause and that `[sandbox]` in harness.ini controls it.
- **Network** (`network = deny`): commands run in a new network namespace via
  `bwrap --unshare-net`, or `unshare --net` if bwrap is not installed.
- **Unsupported systems degrade with a warning**: if the kernel has no
  Landlock or neither bwrap nor unshare works, commands still run, the UI
  shows a `harness sandbox:` warning, commands print a Landlock warning on
  stderr when Landlock is unavailable, and `harness doctor` reports the
  sandbox status.

## Untrusted content

Tool results, project files and web pages reach the model wrapped in
provenance markers, after known secrets are replaced with `[REDACTED]`:

```text
<tool_result tool="bash" call_id="c1" trust="workspace">
...
</tool_result>
```

- `trust="workspace"`: your trusted project.
- `trust="untrusted" origin="untrusted project"`: a project you have not
  trusted (or that changed since you trusted it).
- `<web_content ... trust="untrusted" origin="external">`: web content.
- `trust="untrusted" origin="mcp server"`: the result of an MCP tool,
  whatever the project's trust.

The system prompt tells the model that text inside markers is data, not
instructions. Content cannot close its own marker: marker tags inside it are
escaped.

## For developers

- `harness_policy::rules::PolicyEngine::new(&PolicyConfig::from_config(&cfg), root)`,
  `.with_sink(..)`, `.with_grants(..)`, `.with_overlay(origin, &PolicyOverlay::from(&agent.policy))`,
  then `decide(&Action::from_tool_call(name, &args), mode, cwd)`.
- `harness_policy::approvals::ApprovalBroker::request(Vec<PendingApproval>)` for
  all `ask` calls of one model turn; `RecorderApprovalEvents` writes events;
  `ChannelUi` hands prompts to the TUI.
- `harness_policy::sandbox::SandboxedExec::new(Arc::new(LocalExec::new()), Launcher::current_exe()?, SandboxConfig::from_ini(&cfg.sandbox, root))`
  as the bash tool's `Exec`. The binary must dispatch `harness __sandbox-exec ...`
  to `harness_policy::sandbox::launcher_main` (the crate's `harness-sandbox`
  binary does the same). The launcher opens rule paths with `O_PATH` itself
  (std drops `O_PATH` on musl, which used to lose the `/dev/tty` rule in the
  static release build); `--abi N` targets an older Landlock ABI to test how
  the sandbox degrades, and `sandbox::device_ioctls_restricted()` says
  whether this kernel restricts device ioctls.
- `harness_policy::provenance::Labeler::tool_result(..)` before tool output
  goes into a model request; include `provenance::SYSTEM_PROMPT_SECTION` in
  the system prompt.
