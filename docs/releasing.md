# Releasing

Releases are cut by GitHub Actions and are only ever built from a commit
whose full CI passed. Nothing is published by hand.

## Cutting a release

1. Make sure `CHANGELOG.md` has entries under `## Unreleased` describing
   the release. Those entries become the release notes.
2. In the GitHub **Actions** tab, run **Cut release**
   (`.github/workflows/cut-release.yml`) on the branch to release from
   (normally `main`) and enter the version, `X.Y.Z`. A leading `v` is
   stripped, so `v0.2.0` also works; anything else, such as `vv0.2.0` or
   `0.2`, is rejected before any tag is made.
   From a terminal: `gh workflow run cut-release.yml -f version=0.2.0`.

The workflow then:

1. checks that CI (`ci.yml`) is green on the branch head
   (`scripts/release-ci-gate.sh`; it waits up to 30 minutes for a run that
   is still going and fails if any run on that commit failed or none
   succeeded),
2. bumps `[workspace.package] version` in `Cargo.toml`, the workspace
   crates in `Cargo.lock`, and renames `## Unreleased` in `CHANGELOG.md` to
   `## [X.Y.Z] - YYYY-MM-DD` with a fresh empty `## Unreleased` above it
   (`scripts/release-bump.sh`),
3. commits `Release vX.Y.Z`, creates the annotated tag `vX.Y.Z` and pushes
   both in one atomic push,
4. starts the **Release** workflow for the tag.

A version older than the current one, a version that already has a
CHANGELOG section or tag, or an empty `## Unreleased` section stops the
workflow before anything is pushed.

## The Release workflow

`.github/workflows/release-harness.yml` runs on any pushed `v*` tag, or
when started for a tag (`gh workflow run release-harness.yml --ref
vX.Y.Z`; the Cut release workflow does this because a tag pushed with the
workflow token does not trigger push workflows).

| Job | What it does |
|---|---|
| `ci` | the whole CI workflow (`ci.yml`, called as a reusable workflow) on the tagged commit |
| `meta` | the tag must be `v` + the workspace version (`scripts/release-version.sh`) and the CHANGELOG must have notes for it |
| `build` | `harness` for each target, after `ci` and `meta` pass |
| `helpers` | the static remote helpers (`scripts/build-remote-helpers.sh`, checked with `scripts/verify-remote-helpers.sh`), after `ci` and `meta` pass |
| `publish` | after every job above: verifies every checksum and creates the GitHub release with the notes from `scripts/release-notes.sh` |

If any CI job fails, nothing is built and no release is created. A version
with a pre-release suffix (`1.0.0-rc.1`) is published as a pre-release.
Re-running the workflow for a tag that already has a release replaces its
assets and notes.

### Targets

| Target | Runner | How |
|---|---|---|
| `x86_64-unknown-linux-musl` | `ubuntu-latest` | `cargo zigbuild`, static |
| `aarch64-unknown-linux-musl` | `ubuntu-latest` | `cargo zigbuild`, static; smoke-tested under `qemu-aarch64` |
| `aarch64-apple-darwin` | `macos-latest` | native `cargo build` |
| `x86_64-apple-darwin` | `macos-15-intel` | native `cargo build` |

Every binary is smoke-tested before packaging: `harness --version` must
print exactly `harness X.Y.Z (<commit>)` for the tagged commit
(`scripts/release-smoke.sh`). Linux builds must be statically linked.

## Release assets

Each asset has a `.sha256` file in `sha256sum -c` format.

| Asset | Contents |
|---|---|
| `harness-X.Y.Z-<target>.tar.gz` | `harness-X.Y.Z-<target>/harness`, `README.md`, `CHANGELOG.md` |
| `harness-remote-agent-X.Y.Z-<arch>-linux-musl.tar.gz` | the static remote helper `harness-X.Y.Z-<arch>-linux-musl` and its `.sha256`, ready to drop into `<data_dir>/remote-bin` (see [remote.md](remote.md)) |

Verify and install a download:

```sh
sha256sum -c harness-0.1.0-x86_64-unknown-linux-musl.tar.gz.sha256   # macOS: shasum -a 256 -c
tar -xzf harness-0.1.0-x86_64-unknown-linux-musl.tar.gz
install -m 755 harness-0.1.0-x86_64-unknown-linux-musl/harness ~/.local/bin/harness
harness --version
```

## Version and commit

`harness --version` prints the version and the git commit it was built
from, for example `harness 0.1.0 (1a2b3c4d5e6f)`. `crates/harness/build.rs`
takes the commit from the `HARNESS_GIT_COMMIT` environment variable when
set (release builds set it to the tagged commit), else from `git rev-parse
HEAD`, else prints `unknown` (for example a build from a source archive).

## Scripts

All are POSIX `sh` and run on Linux and macOS; `cargo test -p harness
--test release` exercises them against fixtures and a fake `gh`.

| Script | Purpose |
|---|---|
| `scripts/release-bump.sh VERSION [DATE]` | bump Cargo.toml, Cargo.lock and CHANGELOG |
| `scripts/release-notes.sh VERSION [CHANGELOG]` | print the CHANGELOG section for a version |
| `scripts/release-version.sh [TAG]` | print the workspace version; check a tag matches it |
| `scripts/release-ci-gate.sh SHA [WORKFLOW]` | succeed only if CI is green on a commit |
| `scripts/release-smoke.sh VERSION COMMIT CMD...` | check `CMD --version` output |
| `scripts/release-package.sh OUT NAME FILE...` | write `NAME.tar.gz` and its `.sha256` |

## Repository settings

- The workflows ask for only the token permissions each job needs
  (`contents: write` to push and publish, `actions: write` to start the
  Release workflow). An organization or repository policy that limits the
  workflow token to read-only overrides that: allow *Read and write
  permissions* under **Settings, Actions, General, Workflow permissions**.
- If the release branch is protected against pushes, allow GitHub Actions
  to bypass the rule, or the Cut release push fails (nothing is tagged in
  that case).
