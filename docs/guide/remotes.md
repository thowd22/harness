# Remote hosts

Agents and workflow steps can run on machines you declare in your own
`harness.ini`. harness uses your system OpenSSH (keys, agent, `~/.ssh/config`
and known_hosts), syncs the workspace, runs a small helper on the host and
brings the changes back through the same policy and hygiene checks as local
edits. Credentials never leave your machine.

```ini
[remote.gpu-box]
host = 10.0.0.5
user = ci
labels = gpu, cuda
workdir = /scratch/harness
max_jobs = 2
sync = rsync
policy.bash.deny = shutdown *, reboot *
```

Use a host from an agent or workflow step:

```yaml agent
name: trainer
description: Runs training jobs on the GPU box
prompt: coder
tools: [read, write, edit, bash, todo]
runs_on: { labels: [gpu] }
```

On slow links add `compression = yes`, or keep the default
`compression = auto`, which turns SSH compression on only when the measured
link is slower than `compression_below_mbps` (20 Mbit/s).

`/remote` lists hosts with a health badge, whether compression is active,
and a **test** action;
`harness doctor` checks that every host is reachable. Project files can
refer to hosts but cannot add them. See [remote.md](../remote.md) for sync,
reconnects and security details.
