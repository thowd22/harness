# Effective configuration and config hash

## Viewing the effective config

`harness_config::effective::render` prints every `harness.ini` value after
defaults, one per line, with where it came from:

```text
# effective configuration (/home/me/.config/harness/harness.ini)

[general]
default_model = sonnet        ; default
mode = ask                    ; /home/me/.config/harness/harness.ini:2

[provider.anthropic]
api_key = [redacted]          ; /home/me/.config/harness/harness.ini:5
```

Secrets always print as `[redacted]`. `harness config show [path]` (the
binary calls `effective::run_show`) and the `/config` command show this view;
`/config` also lists the loaded prompts, agents, workflows and skills with
their origin.

## Config hash

`effective::config_hash(&config, Some(&catalog))` is a sha256 over the
canonical JSON of:

- the effective config, without machine-specific paths (the config file
  location, `[general] data_dir` and `[memory] drive`), and
- every active prompt, agent, workflow and skill: kind, name, scope and the
  sha256 of its file.

The same configuration gives the same hash on every machine; changing any
value or definition changes it. Eval results carry it as `config_hash`, so a
baseline comparison shows when the configuration differs
([evals.md](evals.md)); runs are stamped with it so a result can be traced to
the exact configuration that produced it.
