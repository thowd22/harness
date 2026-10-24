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
| harness overhead guard (in test) | ubuntu-latest, macos-latest | `scripts/bench-guard.sh target/debug/harness`: `harness bench --fake`, fails when overhead per call or session start passes its limit |
| cargo-deny | ubuntu-latest | `cargo deny check` (advisories, licenses, bans, sources from `deny.toml`) |

The harness overhead guard runs in the test job after the test suite, on
the dev-profile binary the tests built. It takes the best of 7 runs per
attempt and fails only when 3 attempts in a row are over a limit, so a
noisy runner does not fail it. It needs only POSIX tools (no jq). Its
limits, baseline and how to update them are in docs/performance.md
([CI overhead guard](performance.md#ci-overhead-guard)).

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
scripts/hermetic-guard.sh                              # tests stay off HOME and XDG dirs
cargo build -p harness && scripts/bench-guard.sh       # harness overhead guard
cargo deny check
```

### Hermetic tests

Tests must never read or write a developer's real config, sessions,
memory or credentials. Every test that starts the `harness` binary sets
`HARNESS_CONFIG`, `HOME` and `XDG_CONFIG_HOME`/`XDG_DATA_HOME`/
`XDG_CACHE_HOME`/`XDG_STATE_HOME` to a temp dir (`common::hermetic` in
`crates/harness/tests/common`), and the eval process executor gives each
attempt its own data directory. `scripts/hermetic-guard.sh` checks this:
it runs the `harness` and `harness-eval` suites with `HOME` and every XDG
base directory pointed at an empty sentinel directory (`CARGO_HOME` and
`RUSTUP_HOME` keep the real toolchain) and fails, listing the files, if
anything was created there. `XDG_RUNTIME_DIR` is redirected too but not
checked, since it only holds per-boot SSH control sockets. CI runs the
guard after the test step; pass other cargo arguments to narrow it, e.g.
`scripts/hermetic-guard.sh test -p harness --test headless`.

To catch macOS-only `cfg` errors and warnings from Linux, install the target
(`rustup target add aarch64-apple-darwin`) and run
`cargo clippy --workspace --all-targets --target aarch64-apple-darwin -- -D warnings`.
`aws-lc-sys` needs a C compiler for that target; `zig cc -target aarch64-macos`
works (strip the `-arch`, `--target=` and `-mmacosx-version-min` flags cc
passes it in a small wrapper and point `CC_aarch64_apple_darwin` at it).

To lint the workflow itself, run [actionlint](https://github.com/rhysd/actionlint)
on `.github/workflows/`.

## Dependency updates

Dependabot alerts and security updates are enabled for the repository, so a
vulnerable dependency raises an alert and gets a fix PR automatically.
`.github/dependabot.yml` adds weekly version updates for `cargo` and
`github-actions`, with minor and patch bumps grouped into one PR per
ecosystem. `cargo deny check` in CI still fails a build on known advisories.

Dependabot ignores `dtolnay/rust-toolchain`: its tag is the Rust version CI
installs, so taking its bump would move CI to a compiler nobody pinned. Wire
format crates (`zstd`, `base64`) are checked against golden frames from older
helpers in `crates/harness-remote/tests/wire_compat.rs`, so a bump that
changes what deployed helpers read or send fails the build.

### Upgrading the Rust toolchain

The compiler is upgraded by hand, in one change:

1. Set the new exact version (`x.y.z`) as `channel` in `rust-toolchain.toml`.
2. Change every `dtolnay/rust-toolchain@` tag in `.github/workflows/` to the
   same version. `crates/harness/tests/ci_workflow.rs` fails if any tag
   differs from `rust-toolchain.toml`.
3. Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D
   warnings` and `cargo test --workspace`, and fix new lints and warnings.
   A crate bump that needs a newer Rust than the pin fails CI; raise the
   toolchain first, in its own change.
