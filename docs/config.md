# Configuration: harness.ini

All user configuration, including credentials and keybindings, lives in one
INI file. The full key reference is `backlog/docs/doc-2 - harness.ini-Reference.md`.

## Where the file is

1. `$HARNESS_CONFIG`, if set. The file must exist, or harness stops with an error.
2. Otherwise `$XDG_CONFIG_HOME/harness/harness.ini`.
3. Otherwise `~/.config/harness/harness.ini`.

If there is no file at the default path, harness runs with built-in defaults.

## Syntax

```ini
[section]
key = value            ; inline comment (a `;` after whitespace)
; full-line comment
# also a full-line comment
```

- Section names may contain dots: `[provider.anthropic]`, `[remote.gpu-box]`.
- Keys may contain dots and model-ref characters: `bash.allow`,
  `anthropic/claude-sonnet-5-5`.
- `#` inside a value is not a comment, so `accent = #bd93f9` works.
- Booleans are `true` or `false`.
- Lists are comma-separated: `labels = gpu, bigmem`.
- Durations are seconds, or take a suffix: `20`, `20s`, `500ms`, `5m`, `1h`.
- Paths expand a leading `~/`. `~user/` is not supported.
- Every key must be set at most once, and every section must appear at most once.

## Strict validation

Typos must not quietly change behaviour, so harness rejects anything it does
not know. Each error names the file and line, and suggests a fix when the
name is close to a known one:

```text
~/.config/harness/harness.ini:12: unknown key `defualt_model` in [general] (did you mean `default_model`?)
~/.config/harness/harness.ini:20: unknown section [provider.antropic] (did you mean `provider.anthropic`?)
~/.config/harness/harness.ini:31: invalid value for `theme` in [ui]: expected one of dracula | none, got `solarized`
```

All problems are reported at once.

### `harness config check`

`harness config check [path]` validates the file without starting a
session. It prints `<path>: ok` and exits 0, or prints every error and exits 1.
(The library entry point is `harness_config::ini::run_check`.)

## Credentials

Each `[provider.<id>]` section can supply its API key three ways. The first
one that gives a non-empty value wins:

1. `api_key = sk-...` (inline in the file)
2. `api_key_cmd = pass show anthropic` (runs a command and reads the key from stdout)
3. `api_key_env = ANTHROPIC_API_KEY` (reads an environment variable)

Rules:

- **File permissions.** If the file holds an inline `api_key`, it must be
  readable only by you. harness refuses a file that group or others can read
  and tells you how to fix it:
  `chmod 600 ~/.config/harness/harness.ini`.
- **`api_key_cmd`** runs under `sh -c` with stdin closed and a 10-second time
  limit. Leading and trailing whitespace is trimmed from its output. Its
  stderr is thrown away, never logged, because password managers can print
  sensitive text there. If it fails, harness reports only the exit status.
  Run the command yourself to debug it. If the command fails and
  `api_key_env` gives a value, that value is used.
- **Never shown.** Keys are held in memory that is wiped when it is freed.
  They print as `[redacted]` in logs, debug output, error messages and
  serialized config (`harness config show`).
- **Not inherited.** Environment variables named by `api_key_env` are listed by
  `Config::secret_env_vars()` so tools can remove them from subprocess
  environments.
