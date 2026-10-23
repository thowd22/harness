# Continuous integration

`.github/workflows/ci.yml` runs on every push (branches and tags), every pull
request, on demand (`workflow_dispatch`) and when another workflow calls it
(`workflow_call`). The release workflow only publishes a commit this workflow
passed on (`scripts/release-ci-gate.sh`).

| Job | Runners | Command |
|---|---|---|
| rustfmt | ubuntu-latest | `cargo fmt --all -- --check` |
| clippy | ubuntu-latest, macos-latest | `cargo clippy --workspace --all-targets -- -D warnings` |
| test | ubuntu-latest, macos-latest | `cargo nextest run --workspace --locked --profile ci`, then `cargo test --workspace --locked --doc` |
| cargo-deny | ubuntu-latest | `cargo deny check` (advisories, licenses, bans, sources from `deny.toml`) |

Tests run under [cargo-nextest](https://nexte.st) with the `ci` profile in
`.config/nextest.toml`: a test running longer than 60 s is reported as slow
and killed after 5 minutes, so a hang fails with the test's name instead of
running into the job timeout. nextest does not run doctests, hence the
separate step.

`RUSTFLAGS=-D warnings` is set for every job, so a warning on either OS fails
the run. The toolchain comes from `rust-toolchain.toml`.

## Caching

`Swatinem/rust-cache` caches `~/.cargo` and `target/` per OS and job
(`shared-key: clippy` / `test`). Caches are written only from pushes, never
from pull requests, so a PR cannot poison the cache main and tags use.
`CARGO_INCREMENTAL=0` keeps the cached `target/` small. With a warm cache a
no-change run is dominated by test time and stays well under 10 minutes; a
pull request run in progress is cancelled when a newer commit is pushed to it.

## Host-dependent tests

Hosted runners lack several things a developer machine usually has:

| Feature | ubuntu-latest | macos-latest | Tests that need it |
|---|---|---|---|
| Landlock | usually present (kernel 6.x) | absent (Linux-only) | `harness-policy` `tests/sandbox.rs` |
| bwrap / unshare network isolation | bwrap not installed; `unshare` can be blocked by AppArmor's unprivileged user-namespace restriction | absent | `network_deny_blocks_connections` |
| `/proc` | yes | no | process-group kill checks in `harness-tools` and `harness-remote` |
| sshd on localhost | no | no | `harness-remote` `ssh.rs` real-SSH test, `#[ignore]`d |
| a TTY | no | no | none: TUI tests render to `TestBackend` |
| a short, unsymlinked `$TMPDIR` | yes (`/tmp`) | no: `/var/folders/…/T/` (~50 bytes) under the `/var` -> `/private/var` symlink | remote helper socket tests, path comparisons against `pwd` |

These tests measure what the host supports instead of assuming it, and when
a feature is missing they **replace** the enforcement assertion with an
assertion on the degraded path, never drop it:

- **Landlock missing:** the sandbox tests assert that the write outside the
  workspace is *not* blocked and that a `harness: warning:` line names
  Landlock, and they print `SKIP enforcement check: <reason>` to stderr.
- **No working bwrap/unshare:** the network test asserts that
  `network = deny` degrades with a `network = deny` warning, and logs the
  skip the same way.
- **No `/proc` (macOS):** process liveness is checked with `ps -o stat=`
  (`harness_tools::conformance::local_process_gone`), so a surviving process
  still fails the test. It never counts as dead just because `/proc` is
  missing.

- **Long or symlinked `$TMPDIR` (macOS):** Unix socket paths are capped at
  104 bytes there, so the remote-helper tests put their socket directories
  under `/tmp`; tests that compare a path a shell printed (`pwd`) accept the
  resolved form of the expected directory.

The test job prints which of these features the runner has (bwrap,
unshare, sshd, TTY and the `harness-sandbox --probe` result), then reruns the
sandbox tests with `--nocapture` so their `SKIP` lines show in the log.

## Reproducing CI locally

```sh
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --locked --profile ci   # or: cargo test --workspace --locked
cargo test --workspace --locked --doc
cargo deny check
```

To catch macOS-only `cfg` errors and warnings from Linux, install the target
(`rustup target add aarch64-apple-darwin`) and run
`cargo clippy --workspace --all-targets --target aarch64-apple-darwin -- -D warnings`.
`aws-lc-sys` needs a C compiler for that target; `zig cc -target aarch64-macos`
works (strip the `-arch`, `--target=` and `-mmacosx-version-min` flags cc
passes it in a small wrapper and point `CC_aarch64_apple_darwin` at it).

To lint the workflow itself, run [actionlint](https://github.com/rhysd/actionlint)
on `.github/workflows/`.
