# harness.ini

One file you own: `$HARNESS_CONFIG`, else
`$XDG_CONFIG_HOME/harness/harness.ini` (`~/.config/harness/harness.ini`).
Unknown sections and keys are errors with line numbers. Lists are comma
separated; comments start with `;` or `#`.

```ini
[general]
default_model = sonnet           ; alias or provider/model
mode = auto-edit                 ; ask | auto-edit | auto
identity_prompt = on             ; on | off: the built-in identity prompt layer

[provider.anthropic]
api_key_cmd = pass show anthropic

[provider.openrouter]
api_key_env = OPENROUTER_API_KEY
app_name = harness

[models]
sonnet = anthropic/claude-sonnet-5-5
fast   = anthropic/claude-haiku-4-5

[policy]
bash.allow = cargo build*, cargo test*, git status*, git diff*
bash.ask   = git commit*
bash.deny  = git push --force*
outward.ask = git push*, gh pr create*

[verify]
command = cargo test --workspace

[ui]
timestamps = relative
mouse = true
clipboard = osc52        ; osc52 | system | auto (see docs/tui.md#copy-and-paste)

[memory]
enabled = true
dream = daily
headless_access = false
```

Credentials: `api_key` (inline; the file must then be mode 600),
`api_key_cmd` (stdout of a command) or `api_key_env` (an environment
variable). Keys never appear in logs, events or `harness config show`.
`harness login <anthropic|openai|openrouter>` checks a key with the provider
and writes it for you (mode 600); `harness logout <provider>` removes it.
See [providers.md](../providers.md#logging-in).

Inspect what is in effect, with the source of every value:

```sh
harness config show
```

Each run is stamped with a hash of the effective configuration and the
loaded YAML (see [effective-config.md](../effective-config.md)).
