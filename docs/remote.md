# Remote execution over SSH

harness can run an agent's tools on another machine (more CPU, RAM or a GPU)
while the model loop stays on your machine. Provider calls, credentials,
memory and the event log never leave the local host. The remote host only
receives file operations and processes, through a small helper.

Design: `backlog/docs/doc-6 - Remote-Execution-over-SSH.md`. Code:
`crates/harness-remote`.

## Declaring hosts

Hosts exist only if you declare them in your own `harness.ini`. Project
files (`.harness/*.yaml`) can refer to them with `runs_on`, but they cannot
add hosts.

```ini
[remote.gpu-box]
host = 10.0.0.5            ; required; may be a ~/.ssh/config alias
user = ci                  ; default: your ssh config / login name
port = 22
identity_file = ~/.ssh/id_gpu
labels = gpu, cuda         ; matched by runs_on: { labels: [...] }
workdir = /scratch/harness ; default ~/.cache/harness/work
max_jobs = 2               ; default 1
sync = rsync               ; rsync (default) or git
compression = auto         ; auto (default), yes or no: ssh compression
compression_below_mbps = 20 ; auto: compress links slower than this
policy.bash.deny = shutdown *, reboot *
policy.bash.ask = docker *
```

See the harness.ini reference (doc-2) for the full key list. Unknown host
names are rejected with the list of declared hosts.

### `/remote`

The `/remote` view lists every host with a health badge
(`ok`, `degraded`, `down`, `?` when not probed yet) and its resources, for
example:

```
[ok] gpu-box  ci@10.0.0.5:22  [gpu, cuda]  max_jobs=2  sync=rsync  compression=off (auto: link 940 Mbit/s >= 20.0 Mbit/s)  32 cpu, 200/251 GiB mem free, 2 gpu (NVIDIA A100), 812 GiB disk free, 0 jobs
```

Its **test** action measures the link, connects, checks the helper version
and probes the host:

```
gpu-box: ok
  connection: ok (41 ms)
  helper: 0.1.0 (x86_64)
  resources: 32 cpu, ...
  link: 940 Mbit/s
  compression: off (auto: link 940 Mbit/s >= 20.0 Mbit/s)
  rsync compression: zstd
```

## Transport: system OpenSSH

harness runs your system `ssh`, so `~/.ssh/config`, `ssh-agent`, `ProxyJump`,
certificates, hardware keys and `known_hosts` work exactly as in your shell.
Every invocation adds:

| Option | Why |
| --- | --- |
| `ControlMaster=auto`, `ControlPath=$XDG_RUNTIME_DIR/harness/cm-%C`, `ControlPersist=60` | one multiplexed connection per host; the socket directory is created with mode 0700 (fallback `/tmp/harness-$USER`) |
| `StrictHostKeyChecking=yes`, `BatchMode=yes` | unknown hosts fail instead of prompting |
| `ServerAliveInterval=15`, `ServerAliveCountMax=3`, `ConnectTimeout=10` | a dead network is noticed within about 45 s |
| `ForwardAgent=no`, `-a`, `ForwardX11=no`, `-x`, `ClearAllForwardings=yes`, `SendEnv=-*`, `PermitLocalCommand=no` | nothing is forwarded to the remote host |
| `Compression=yes` or `Compression=no` | per host, see [Compression](#compression); compressed connections use their own master socket, `cmz-%C` |

### Compression

SSH channel compression (zlib) saves time on slow links and costs time on
fast ones, so it is set per host with `compression`:

- `yes`: always compress (`Compression=yes` on the host's ControlMaster).
- `no`: never compress (`Compression=no`).
- `auto` (default): compress only when the host's last measured link
  throughput is below `compression_below_mbps` (default 20 Mbit/s). A host
  that has not been measured is not compressed.

The link is measured by the `/remote` **test** action and, when a remote
step connects to an `auto` host with no measurement from the last 24
hours, before that connection is opened. A measurement first runs an
untimed empty command to bring the connection up (so the SSH handshake is
not counted), then times a second empty command as the per-command
overhead and 256 KiB of random (incompressible) bytes over ssh, so it
reports the raw link speed. A link that connects but cannot move the bytes
in 10 s is recorded as slower than 0.2 Mbit/s. If the connection itself
fails or does not come up within 10 s, nothing is recorded and the host
keeps its previous measurement (or stays unmeasured, so uncompressed).
Measurements are kept in
`<data_dir>/remote-links.json`. Each connection logs the decision as a
`remote_op` with op `link.compression` (and `link.measure` when it
measured).

A multiplexed client cannot change its master's compression, so
compressed and uncompressed connections use separate ControlMaster
sockets (`cm-%C` and `cmz-%C`): switching never disturbs a running
master.

Workspace sync over rsync also compresses (`--compress`). When both the
local and the remote `rsync --version` list `zstd` (rsync 3.2 or later),
harness adds `--compress-choice=zstd`; otherwise rsync uses its default.

The helper protocol compresses its own frames with zstd (see
[Wire format](#wire-format)) only when the SSH channel does not: on a
host whose connection uses `Compression=yes` (by `compression = yes` or an
`auto` decision) the client does not offer helper compression, so data is
never compressed twice.

### Unknown or changed host keys

harness never accepts a host key on first use. If the key is unknown you
get:

> ssh: the host key for gpu-box is not in known_hosts. harness never accepts
> unknown host keys: verify the fingerprint, then add it (for example `ssh
> gpu-box` once, or `ssh-keyscan gpu-box >> ~/.ssh/known_hosts` after
> checking it)

A changed key is reported as a possible man-in-the-middle attack and
harness refuses to connect until you fix `known_hosts`. Authentication,
DNS and reachability failures each get their own message.

### Decision: OpenSSH instead of a Rust SSH client (spike, TASK-100)

We compared shelling out to OpenSSH with embedding `russh`:

| | OpenSSH (`ssh`) | russh |
| --- | --- | --- |
| `~/.ssh/config` (Host aliases, Match, Include) | full | would need our own parser, partial at best |
| ssh-agent, FIDO/hardware keys, certificates | yes | agent yes; FIDO and certificates limited |
| ProxyJump / ProxyCommand | yes | must be reimplemented |
| known_hosts (hashed entries, `@cert-authority`, `@revoked`) | yes | partial |
| multiplexing | ControlMaster, shared with the user's own sessions | in process only |
| dependency weight | none (system binary) | large crypto dependency tree |
| control over the channel | stdio pipes | full API |

The user's existing SSH setup must keep working unchanged, and that decided
it: harness uses the system `ssh`. The one thing a library would offer, a
typed channel, is covered by the helper's JSON-RPC protocol on the ssh
stdio. The cost is a dependency on an `ssh` binary on the local machine,
which every target platform has.

## The helper

The helper is the harness binary itself in helper mode:

```
harness remote-agent --stdio [--session ID] [--grace SECS] [--socket-dir DIR] [--no-compression] [--content-cache BYTES]
```

It speaks versioned JSON-RPC 2.0 on stdin/stdout (protocol version 2,
compatible with 1): `hello`, `ping`, `fs.read`, `fs.write`,
`fs.replace_atomic`, `fs.set_mode`, `fs.stat`, `fs.list`, `fs.glob`,
`exec.start`/`exec.kill`/`exec.attach`/`exec.release`/`exec.list` plus the
`exec.output`/`exec.exit` notifications, `probe`, and `sync.manifest`,
`sync.changes`, `sync.remove`, `sync.prune`. Edits are computed locally
(stale-read checks and diffs stay local) and applied with
`fs.replace_atomic`. `--no-compression` makes the helper decline zstd.
`--content-cache BYTES` sizes the helper's content cache (default 64 MiB,
`0` turns it off); see [Content cache](#content-cache).

### Wire format

The `hello` exchange negotiates the protocol version and compression. The
client sends `protocol: 1` with `max_protocol: 2` and, unless the SSH
channel compresses, a zstd offer (level and threshold). A protocol 2
helper answers `protocol: 2` and `compression: "zstd"`; a protocol 1
helper ignores the new fields and answers `protocol: 1`, and the
connection stays on protocol 1 (uncompressed JSON lines with base64
payloads). Both sides switch framing right after the `hello` answer.

Protocol 2 framing:

- A message without binary data and under the compression threshold is
  one JSON line, exactly as in protocol 1.
- Every other message is a binary frame: a header line
  `~<json_len> <data_len|-> <wire_len> <raw|zstd>` followed by
  `wire_len` bytes. Decoded, the body is the JSON message followed by the
  raw payload (file contents for `fs.read`/`fs.write`/`fs.replace_atomic`,
  output for `exec.output`), so payloads are never base64-encoded.
- With zstd negotiated, a frame whose body is 4 KiB or more is compressed
  at zstd level 1; smaller frames, and frames zstd cannot shrink
  (already-compressed or random data), are sent as-is.
- The 64 MiB frame limit applies to the decoded size declared in the
  header, checked before decompressing, and decompression is bounded by
  it, so a small compressed frame cannot expand into a decompression bomb.

`cargo bench -p harness-remote --bench wire` reports bytes on the wire
and latency for a read, a write and a streaming exec (`cat`) of 1 MiB of
source-like text over simulated 10, 100 and 1000 Mbit/s links (1 ms
one-way latency). On one development machine:

| link | op | protocol 1 (base64) | protocol 2, no zstd | protocol 2, zstd |
|---|---|---|---|---|
| 10 Mbit/s | read | 1.40 MB, 1130 ms | 1.05 MB, 845 ms | 0.15 MB, 126 ms |
| 10 Mbit/s | exec | 1.42 MB, 1141 ms | 1.07 MB, 862 ms | 0.22 MB, 181 ms |
| 100 Mbit/s | write | 1.40 MB, 119 ms | 1.05 MB, 89 ms | 0.15 MB, 19 ms |
| 1000 Mbit/s | read | 1.40 MB, 19 ms | 1.05 MB, 11 ms | 0.15 MB, 9 ms |
| 1000 Mbit/s | exec | 1.42 MB, 19 ms | 1.07 MB, 16 ms | 0.21 MB, 11 ms |

The generated text compresses better (about 7 times) than typical source
code (about 3 to 5 times), so expect smaller gains on real trees.

### Content cache

Agents read the same files again and again, and often write back content
the host already has (a copy, a revert). For each session the client and
the helper each keep a cache of file contents keyed by their sha256, so
such content crosses the link as a 64-character hash instead of the bytes:

- **Repeat reads.** The client sends the hash of the copy it holds for the
  path (`fs.read` `have`). The helper always reads the file; if it still
  has that hash, the answer is `cached: true` with the hash and no bytes.
  A file changed on the host in any way has a new hash and comes back in
  full.
- **Reads after writes.** Content the client wrote is cached too, so
  reading it back is a hash.
- **Writes of known content.** When the helper read or received the same
  content earlier in the session, the client sends `fs.write` (or
  `fs.replace_atomic`) with `hash` and `cached: true`. The helper writes its
  cached copy.

The caches are bounded (64 MiB per side by default, least recently used
entries go first, and a file larger than a quarter of the cache is never
cached). The helper's cache lives as long as its session, so it survives
reconnects. Nothing can go wrong silently:

- if the helper no longer holds the content (evicted, or a new helper
  after a restart), it answers `CACHE_MISS` and the client sends the bytes;
- if the client's copy was evicted or no longer matches its hash, it reads
  the file again in full;
- bytes sent with a `hash` that does not match are refused
  (`HASH_MISMATCH`) before anything is written, and the client resends
  them without the hash.

None of these is an error: the operation succeeds with a full transfer,
and tools return exactly what they return without the cache. The secret
guard checks write contents whichever way they travel. Each `remote_op`
event for `fs.read`, `fs.write` and `fs.replace_atomic` records the bytes
the cache saved as `bytes_saved` (omitted when 0).

The helper advertises its cache size in `hello` (`content_cache`); a
helper without one (older helpers, `--content-cache 0`) never receives a
write by hash. The client cache size is `ClientOptions::content_cache` in
the library; there is no `harness.ini` key.

### Installing and upgrading

The helper is uploaded on demand to `~/.cache/harness/bin/<version>-<arch>`:

1. `uname -m` picks the build (`x86_64` or `aarch64`).
2. The sha256 of the installed file is compared with the local build. If it
   matches, nothing is uploaded.
3. Otherwise the build is streamed into a temporary file, its sha256 is
   verified on the remote side, it is made executable (mode 0700) and moved
   into place. Older versions for that architecture are deleted.
4. If the running helper still reports another version in its handshake,
   it is uploaded once more and the connection retried.

Local builds are looked up in `$HARNESS_REMOTE_BIN_DIR`, or
`<data_dir>/remote-bin`, as `harness-<version>-<arch>-linux-musl` with an
optional `harness-<version>-<arch>-linux-musl.sha256` file, which must
match. Any build that serves `remote-agent --stdio` works: the full
`harness` binary, or the smaller helper-only `harness-remote-agent` binary
(same protocol, no TUI or provider code). Build them statically with musl:

```sh
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
scripts/build-remote-helpers.sh ~/.local/share/harness/remote-bin x86_64 aarch64
```

The script builds `harness-remote-agent` for each target (with
`cargo-zigbuild` when installed, which the aarch64 cross build needs) and
writes each file with its `.sha256` sidecar. A cross build needs
`cargo install cargo-zigbuild` and `zig` on `PATH` (for example
`pip install --user ziglang`); no root access is required.

Check a build directory before using it:

```sh
scripts/verify-remote-helpers.sh ~/.local/share/harness/remote-bin x86_64 aarch64
```

It checks each sidecar, the ELF architecture, that the binary is static (no
interpreter, no shared libraries) and that it answers the `hello` handshake
with the expected protocol version, architecture and version. A foreign
architecture runs under `qemu-<arch>` (from `qemu-user`, or the path in
`QEMU_AARCH64` / `QEMU_X86_64`); without qemu that run is skipped, unless
`REQUIRE_RUN=1` makes it a failure.

The `Remote helpers` GitHub workflow (`.github/workflows/release.yml`, on
demand) builds both architectures, runs the verify script with
`REQUIRE_RUN=1` under qemu, and uploads the `remote-bin` directory as an
artifact. Tagged releases build and publish the helpers in
`.github/workflows/release-harness.yml`, after CI passes.

### Shell fallback

On hosts where uploading binaries is not allowed, a degraded POSIX shell
fallback offers read, write, atomic replace, chmod, stat, list and exec
using `cat`, `stat`, `mkdir`, `mv`, `chmod`, `ps` and `kill`. Its limits:
there is no `glob` (use `find` in bash), a missing program shows up as exit
code 127 instead of a spawn error, a disconnect cannot be reattached, and
GNU or BSD `stat` is required.

## Workspace sync

Before a remote node starts, the local worktree (tracked and untracked
files, honouring `.gitignore`, never `.git/`) is copied to
`<workdir>/<project-key>/<run-id>`, where the project key is
`<dir name>-<12 hex of sha256(path)>`:

- `sync = rsync`: `rsync --archive --compress --files-from=-` over the same
  ssh options as above, with `--compress-choice=zstd` when both ends
  support zstd (see [Compression](#compression)).
- `sync = git`: a git bundle of a snapshot commit, built in a temporary
  index (your index, HEAD and branches are untouched), uploaded and checked
  out. Use the repository root as the project root.

harness then checks that the remote copy matches what was pushed.

When the node finishes, the helper reports every added, modified and
deleted file. The change set is applied locally **only if all of these
hold**:

- every file passes the local policy, exactly as if a local `write` tool
  wrote it (protected paths such as `.harness/` and `.git/` are denied; in
  `ask` mode the changes need your approval);
- every file passes the hygiene check (no temp files such as `*.orig`,
  `*.rej`, `*~`, `*.swp`, `core`; inside the agent's `scope` when one is set);
- no file was changed locally while the node ran. A concurrent local edit
  is reported as a conflict and never overwritten.

Otherwise nothing is applied, and the problems are listed. The change set
can still be saved as a unified diff for review.

Remote run directories are removed after a successful node and kept after
a failure for inspection (`on-success`; the other policies are `always` and
`never`). Pruning removes old run directories of a project, by default
keeping the 5 most recent and anything newer than 7 days. Removal never
leaves `<workdir>/<project-key>`.

## Running workflow steps on a host

A workflow step with `runs_on` (or an agent step whose agent has one) runs
its tools on the host it is placed on. Each placed step gets its own
connection and run directory per attempt,
`<workdir>/<project-key>/<run id>-<step>-a<attempt>` (a retry starts from a
fresh copy; a failed attempt's directory is kept for inspection):

1. connect, then push the worktree as above;
2. run the step: agent steps keep the model loop, credentials, memory,
   policy and approvals local and run `read`, `write`, `edit` and `bash`
   there (subagents they spawn too); shell and verify commands run there
   in the host's login environment, with nothing from the local
   environment but `$ARTIFACTS_DIR`; shell artifacts are written to
   `<run dir>.artifacts` on the host and copied back;
3. pull the changes and apply them through the local policy and hygiene
   checks, all or nothing. If they cannot be applied the step fails and the
   change set is saved as `remote-changes.patch` in the step's artifact
   directory;
4. clean up and close the connection.

`bugfix-proof` and `dream` steps cannot run remotely yet. See
[workflows.md](workflows.md#remote-placement-runs_on).

## When the network drops

- ssh keepalives and a helper ping every 15 s detect a dead connection.
- Running processes keep running on the remote host. harness reconnects (3
  attempts, with backoff) using the same helper session. The new ssh session
  finds the running helper through its private Unix socket
  (`$XDG_RUNTIME_DIR/harness/helper-<session>.sock` or
  `~/.cache/harness/run/`, directory mode 0700) and resumes every job from
  the last output it saw, with no gaps and no duplicates. If output had to
  be dropped while disconnected (more than 16 MiB per job), a note says so.
- If the helper is gone, or harness cannot reconnect, the step fails
  (`process supervision ended without an exit status`) and can be retried
  under the workflow's `retries`.
- Cancelling a step kills the remote process group. A helper left without
  a client kills all its process groups after a grace period (120 s by
  default) and exits, so nothing is orphaned.

Connection events (connecting, connected, disconnected, reconnecting,
failed, job lost) carry the host name and drive the host badge.

## Security

- No agent, X11 or port forwarding, and no environment forwarding.
  Workflow steps and agent tools on a host never send the local
  environment: shell and verify steps use the host's login environment
  plus `$ARTIFACTS_DIR`, and agent `bash` starts from the remote login
  `HOME`, `PATH`, `USER`, `SHELL` and locale plus the non-interactive
  settings. Local tokens such as `GITHUB_TOKEN` or `AWS_*` stay local.
- Whatever variables a caller does pass explicitly are scrubbed. harness credentials,
  configured `api_key_env` variables, every `HARNESS_*` variable and any
  value containing a resolved key are removed. Local machine variables
  (`HOME`, `PATH`, `USER`, `SHELL`, locale) are replaced with the remote
  login values, and the rest (`SSH_*`, `XDG_*`, `DISPLAY`, ...) are dropped.
- Secrets are never sent. Every outgoing request, and every file payload
  before it is encoded, is checked against the resolved credentials. A
  write or command that contains one is refused with "contains a harness
  credential". Tests check this on the wire protocol.
- Per-host `policy.bash.deny` and `policy.bash.ask` only add rules on top of
  the global policy, for agents whose tools run on that host. A host cannot
  allow anything that the global policy denies or asks about.
- Every remote operation produces an audit event with its host, operation,
  target, outcome and duration. For a workflow step on a host, the events
  go to the run log as `remote_op` events on the step's node, next to its
  `remote_connection` events. Elsewhere (`/remote test`) they are logged to
  target `harness::remote::audit`.

## Environment variables

| Variable | Meaning |
| --- | --- |
| `HARNESS_REMOTE_BIN_DIR` | directory of local static helper builds (default `<data_dir>/remote-bin`) |
| `XDG_RUNTIME_DIR` | parent of the ControlMaster and helper socket directories |
