# harness doctor

`harness doctor` checks configuration and environment and prints one line per
finding, with a fix for every problem. It exits 1 when something is broken.

```text
[ok  ] config: /home/me/.config/harness/harness.ini is valid
[warn] credentials: /home/me/.config/harness/harness.ini (mode 644) is readable by other users
       fix: chmod 600 /home/me/.config/harness/harness.ini
[FAIL] provider anthropic: auth: none (no API key)
       fix: run `harness login anthropic`, or set api_key_env, api_key_cmd or api_key in [provider.anthropic]
[ok  ] provider openrouter: auth: API key (api_key_env OPENROUTER_API_KEY); key resolves; https://openrouter.ai/api/v1 reachable in 41 ms
[ok  ] sandbox: filesystem sandbox: landlock, fully enforced; ...
[FAIL] remote gpu-box: ssh: connect to host 10.0.0.5 port 22: Connection refused
       fix: check `ssh 10.0.0.5` works without a password prompt (keys in ssh-agent, host in known_hosts), or fix [remote.gpu-box]
[ok  ] mcp github: github-mcp-server 1.2.0; MCP 2026-07-28 (modern); 31 tool(s); ready in 412 ms
[ok  ] lint: prompts, agents and skills pass prompt lint
[warn] definitions: agent `coder`: global ~/.config/harness/agents/coder.yaml shadows builtin:agents/coder.yaml
       fix: rename one of them if both should exist; otherwise nothing to do
2 problem(s), 2 warning(s)
```

| Area | What is checked |
|---|---|
| `config` | `harness.ini` parses and validates (every error with file:line) |
| `credentials` | the file is private (fails if it holds inline `api_key` values and others can read it) |
| `provider <id>` | the auth method in use (`auth: API key (api_key in harness.ini)`, `(api_key_cmd)`, `(api_key_env VAR)` or `none`), that the key resolves, and that the endpoint accepts a TCP connection |
| `sandbox` | Landlock and network isolation support (from the sandbox launcher's probe) |
| `remote <name>` | `ssh <host> true` succeeds in batch mode |
| `mcp <name>` | each enabled `[mcp.<name>]` server starts, completes the handshake and lists its tools (disabled ones are skipped); see [mcp.md](mcp.md) |
| `lint` | prompt lint over every prompt, agent and skill (built-in, global, trusted project) |
| `definitions` | definitions that shadow others of the same name |

Library: `harness_eval::doctor::{diagnose, run, Input, Probes, LiveProbes,
Report, Check, Status}`. Network and process probes go through the `Probes`
trait so the checks are tested offline.
