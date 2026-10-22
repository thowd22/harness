---
id: doc-6
title: Remote Execution over SSH
type: specification
created_date: '2026-10-22 14:26'
updated_date: '2026-10-22 21:56'
---
# Remote Execution over SSH

Agents can run their tools on other machines (more CPU/RAM/GPU) while the model loop stays local.

## Model

- **Local brain, remote hands.** Provider calls, credentials, memory and the event log stay local. A remote node only receives tool operations (fs read/write/edit, exec).
- **Transport:** system OpenSSH (`ssh`), so `~/.ssh/config`, ssh-agent, ProxyJump and known_hosts all work. Connections are multiplexed with ControlMaster (`ControlPath` under `$XDG_RUNTIME_DIR/harness/`). Strict host key checking is on; unknown hosts are rejected (no TOFU prompt inside the agent flow).
- **Helper:** the same binary in helper mode, `harness remote-agent --stdio`, speaks versioned JSON-RPC over the SSH channel: `fs.read`, `fs.write`, `fs.edit`, `fs.stat`, `fs.list`, `exec.start/stream/kill` (process groups, timeouts), `probe`, `sync.*`. Static musl builds for x86_64/aarch64 are uploaded to `~/.cache/harness/bin/<version>-<arch>` and checksum-verified; a degraded POSIX-shell fallback exists for hosts where upload is not allowed.
- **Workspace:** `Workspace`/`Exec` traits have Local and Remote impls, so tools are unchanged.

## Workspace sync

1. Before a remote node starts, the local worktree (tracked + untracked, honoring .gitignore) is synced to `<workdir>/<project-key>/<run-id>` via rsync (default) or git bundle.
2. Tools operate there.
3. On node completion, changes come back as a patch, pass the local policy + hygiene checks (scope, temp files), and are applied to the local worktree (or kept as a branch/patch when the user prefers review first).
4. Conflicts with concurrent local edits are surfaced, never silently overwritten.

## Config

INI (`[remote.<name>]`): host, user, port, identity_file, labels, workdir, max_jobs, sync, compression (`auto` | `yes` | `no`) with compression_below_mbps, per-host tighter policy.

Compression (TASK-129): `yes`/`no` set `Compression=yes`/`no` on the host's ControlMaster; `auto` compresses only when the last measured link throughput (probe: timed burst of random bytes over ssh, persisted in `<data_dir>/remote-links.json`, re-measured on connect after 24 h) is below `compression_below_mbps` (default 20). Compressed masters use a separate ControlPath (`cmz-%C`) because a mux client cannot change its master's compression. rsync adds `--compress-choice=zstd` when both `rsync --version` outputs list zstd, else its default compression. `/remote` and its test action show the decision.
YAML (`runs_on` on agents and workflow steps):

```yaml
runs_on: gpu-box                         # specific host
runs_on: { labels: [gpu], min_mem_gb: 64, fallback: local }   # selector
```

The scheduler matches labels and probe results (cpu, mem, gpu via nvidia-smi, disk, current jobs) and respects `max_jobs`.

## Reliability & security

- Keepalives; on disconnect, reattach to running remote processes when the helper is still alive, otherwise fail the step (eligible for `retries`) and clean up remote processes.
- No agent forwarding, no env forwarding; remote exec env is scrubbed; secrets never sent.
- Hosts usable only if declared in INI (project YAML cannot add hosts).
- Every remote op is an event (host-tagged) so it shows in the map and detail views.
## Decisions (implementation, TASK-100..107)

- **OpenSSH over russh** (TASK-100 spike): keeps `~/.ssh/config`, agent, FIDO keys, certificates, ProxyJump and known_hosts semantics intact with no crypto dependency tree; the typed channel a library would give is provided by the helper protocol. Comparison table in `docs/remote.md`.
- **Framing:** JSON-RPC 2.0. Protocol 1: newline-delimited JSON, binary payloads base64. Protocol 2 (TASK-130): plain messages stay JSON lines; messages with binary payloads or bodies of 4 KiB or more are binary frames (`~<json_len> <data_len|-> <wire_len> <raw|zstd>` header line, then the JSON and raw payload bytes), zstd level 1 when negotiated, never when SSH compression is on for the host. The frame limit applies to the declared decompressed size. `hello` carries the protocol range (`protocol` 1 to `max_protocol` 2), the compression offer and harness version; a protocol 1 helper answers 1 and the connection stays uncompressed. Bytes and latency per link speed: `cargo bench -p harness-remote --bench wire`.
- **Content cache (TASK-131):** client and helper each keep a bounded per-session cache of file contents keyed by sha256 (default 64 MiB per side, LRU, entries up to a quarter of it; the helper's lives with its session, across reattaches). `fs.read` carries `have` (the hash the client holds for the path) and the helper, which always reads the file, answers `cached: true` with the hash and no bytes when it matches; answers carry `hash`. Writes of content the helper holds (it read or received it this session, per the client's model of its cache sized from `hello.content_cache`) send `hash` + `cached: true` and no bytes; writes with bytes carry `hash`, checked by the helper. `CACHE_MISS` (-32040), `HASH_MISMATCH` (-32041), an evicted or corrupt client copy (verified before use) and a helper without a cache all fall back to a full transfer, never an error, so tool results are unchanged. `remote_op` events carry `bytes_saved`. Helper flag `--content-cache BYTES` (0 = off); no harness.ini key.
- **fs.edit is not a wire op:** edits are computed locally (stale-read tracking, diffs, change ledger stay local) and applied with `fs.replace_atomic`.
- **Reattach:** each client owns a helper session id; the helper listens on a private Unix socket (`helper-<session>.sock`, dir 0700) and keeps jobs and up to 16 MiB of output per job across disconnects. A reconnecting `remote-agent --stdio --session <id>` proxies to it; the client resumes with `exec.attach{job, from_seq}`. No client within the grace period (default 120 s) kills every job's process group.
- **Env:** local host-specific variables are never forwarded; the helper substitutes the remote login values of HOME/PATH/USER/LOGNAME/SHELL/TMPDIR/LANG/LC_*.
- **Sync apply is all-or-nothing:** any policy denial, hygiene rejection, missing approval or conflict with a local edit aborts the apply; the change set can be rendered as a patch.
- **Placed nodes (TASK-102):** a workflow step with `runs_on` (or an agent step whose agent declares one) opens a `harness_remote::node::RemoteNode` per step: connect, push to `<run-id>-<node>-a<attempt>` (a fresh dir per attempt, so retries never inherit a failed attempt's tree or artifacts), tools over `RemoteWorkspace`/`RemoteExec`, pull, gated apply (local policy + hygiene with the step scope; `ask` counts as settled because agent tool calls already passed the policy gate), cleanup. Agents get remote read/write/edit/bash through `ChildRequest.host` (`ChildHost`), inherited by their subagents, and the host's `policy.bash.*` overlay through a per-host gate. The local environment is never sent: placed shell/verify steps run in the host's login environment plus `$ARTIFACTS_DIR` (and only variables the step declares), and agent bash on a host starts from an empty base env, so it gets only the remote login `HOME`/`PATH`/`USER`/`SHELL`/`LANG` plus the non-interactive variables. Shell artifacts go to `<run dir>.artifacts` on the host (outside the pulled tree) and are copied back. Unapplied changes fail the step and are saved as `remote-changes.patch`. Step, agent and subagent nodes carry `node_started.host`; `remote_op` and `remote_connection` events are recorded on the step node. bugfix-proof and dream steps stay local and fail clearly when placed.
