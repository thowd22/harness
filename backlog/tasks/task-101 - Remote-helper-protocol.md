---
id: TASK-101
title: Remote helper protocol
status: Done
assignee:
  - '@remote'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-24 01:16'
labels:
  - remote
milestone: m-10
dependencies:
  - TASK-100
  - TASK-20
documentation:
  - backlog/docs/doc-6 - Remote-Execution-over-SSH.md
priority: high
type: feature
ordinal: 101000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Shell quoting over ssh is fragile. A small JSON-RPC helper gives reliable file ops and process control.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 harness remote-agent --stdio implements versioned JSON-RPC for fs, exec, probe and sync
- [x] #2 Static musl builds for x86_64 and aarch64 are uploaded on demand and checksum-verified
- [x] #3 Version mismatch triggers upgrade
- [x] #4 A POSIX shell fallback supports basic ops where upload is not allowed
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [x] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [x] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [x] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. protocol.rs: versioned NDJSON JSON-RPC types and framing. 2. helper.rs: server over stdio reusing LocalWorkspace/LocalExec, jobs that outlive connections. 3. Binary entry: harness remote-agent --stdio (+ helper-only bin). 4. install.rs: arch detection, on-demand upload with remote checksum verification, upgrade on version change or handshake mismatch. 5. shell.rs: POSIX shell fallback Workspace/Exec. 6. Tests + docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Protocol (protocol.rs): newline-delimited JSON-RPC 2.0. PROTOCOL_VERSION=1 and HELPER_VERSION is the crate version. Binary payloads are base64. Frames are capped by MAX_FRAME. The methods are hello, ping, fs.{read,write,replace_atomic,set_mode,stat,list,glob}, exec.{start,kill,attach,release,list}, the notifications exec.output and exec.exit (each with a per-job seq), probe, and sync.{manifest,changes,remove,prune}. WsError maps to and from RPC error codes losslessly.

Helper (helper.rs): Helper::serve(reader, writer), wait_idle, run_stdio and main(args). The helper reuses harness_tools::local::{LocalWorkspace, LocalExec}, so remote behaviour equals local behaviour.

Binary entry points:
- `harness remote-agent --stdio` is crates/harness/src/remote_agent.rs plus a one-line dispatch in main.rs. It runs before harness.ini, logging or the TUI are touched.
- The helper-only bin is harness-remote-agent.

Install (install.rs): Arch, HelperBinaries (looked up in $HARNESS_REMOTE_BIN_DIR or <data_dir>/remote-bin, harness-<v>-<arch>-linux-musl plus an optional .sha256 sidecar), ensure_helper and connect_with_upgrade. An upload goes to a temp file, is sha256-verified on the remote side, is chmod 700 and moved into place, and older versions for that arch are removed. A handshake VersionMismatch forces one re-upload.

Shell fallback (shell.rs): RemoteShell (LocalShell, SshShell, AuditedShell), ShellWorkspace (read/write/replace_atomic/set_mode/stat/list; no glob) and ShellExec (the remote process reports its pgid, and kill/timeout/drop kill the remote group).

Tests:
- protocol::tests and helper::tests.
- tests/conformance.rs (helper process over a pipe).
- tests/install.rs: on-demand upload, checksum, tamper repair, version upgrade removes the old one, corrupted upload rejected, upload disabled, connect via the uploaded helper, handshake mismatch -> forced re-upload.
- shell::tests.
- crates/harness/tests/remote_agent.rs: binary-level hello.

Manual check: `cargo build --release -p harness-remote --bin harness-remote-agent --target x86_64-unknown-linux-musl` produced a static-pie binary that runs.

REMAINING (AC#2): the aarch64 musl build was not verified (the target is not installed here), and no CI/release job produces and publishes the two static builds. Add a CI job that builds harness-remote-agent (or harness) for x86_64- and aarch64-unknown-linux-musl, for example with cargo-zigbuild, and writes the .sha256 sidecars. docs/remote.md documents the commands. Upload and verification of whatever build is present is implemented and tested.
cargo test -p harness-remote: 91 tests pass (64 unit, 27 integration in tests/{conformance,failures,install,sync,tools_remote}.rs), 1 #[ignore] real-sshd test; cargo test -p harness --test remote_agent: 2 pass; cargo clippy -p harness-remote --all-targets -- -D warnings clean.

Integrator update: added scripts/build-remote-helpers.sh (builds harness-remote-agent per musl target, writes harness-<version>-<arch>-linux-musl plus .sha256 sidecar in the layout HelperBinaries::locate expects) and .github/workflows/release.yml (tags/workflow_dispatch: both targets via cargo-zigbuild, sha256sum -c and a static-link check, uploads remote-bin). Verified locally for x86_64 (static-pie, sidecar OK). REMAINING (AC#2, DoD#1): the aarch64 build has not been run (cargo-zigbuild and the aarch64 rustup target are not installed here and installing needs the network); run the Remote helpers workflow once, then check AC#2 and DoD#1. Upload-on-demand and checksum verification are already tested in harness-remote tests/install.rs.

Build lane (aarch64 verification): installed user-local tooling (rustup target aarch64-unknown-linux-musl, cargo-zigbuild 0.24.0; zig 0.16.0 already on PATH). scripts/build-remote-helpers.sh OUT aarch64 and OUT x86_64 both built via cargo-zigbuild: harness-0.1.0-aarch64-linux-musl (ELF AArch64, statically linked, 3.0 MB) and harness-0.1.0-x86_64-linux-musl (statically linked, 3.4 MB); sha256sum -c on both sidecars OK. The aarch64 binary was run under a user-local qemu-aarch64 (extracted from the qemu-user .deb with apt-get download + dpkg-deb -x, no sudo): hello returned arch=aarch64 protocol=1 version=0.1.0, and probe/fs.stat answered. Added scripts/verify-remote-helpers.sh (sidecar match, ELF machine, no PT_INTERP/NEEDED, hello handshake natively or under qemu-<arch>/QEMU_<ARCH>, REQUIRE_RUN=1 makes a missing qemu a failure); negative-checked with a tampered binary (fails on sidecar) and with no qemu + REQUIRE_RUN=1 (fails). release.yml now installs qemu-user and runs the verify script with REQUIRE_RUN=1 for both arches. docs/remote.md documents the tooling and the verify script. cargo test -p harness-remote: 92 pass, 1 ignored (real sshd). The GitHub Remote helpers workflow itself has not been run (no push from here).

The aarch64 musl helper is now built, static-checked and published by the Release workflow: https://github.com/thowd22/harness/releases/tag/v0.1.0 (harness-remote-agent-0.1.0-aarch64-linux-musl.tar.gz plus .sha256), run https://github.com/thowd22/harness/actions/runs/39896827525.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered the harness-remote helper: harness remote-agent --stdio (and the harness-remote-agent bin) speaks versioned NDJSON JSON-RPC (protocol.rs: hello, fs.*, exec.*, probe, sync.*). install.rs uploads static musl helpers on demand with remote sha256 verification and re-uploads on version or handshake mismatch. shell.rs provides a POSIX shell fallback Workspace/Exec for hosts where upload is not allowed. Static x86_64 and aarch64 musl builds come from scripts/build-remote-helpers.sh and are checked by scripts/verify-remote-helpers.sh in release.yml; both were published in v0.1.0 (https://github.com/thowd22/harness/releases/tag/v0.1.0, run 39896827525; commit dbb9f45). Verified: cargo nextest -p harness-remote conformance/install/wire_compat plus protocol:: and shell:: unit tests (30/30 pass) and -p harness remote_agent pass; main CI green at cfb7027 (run 40048623347). Docs: docs/remote.md.
<!-- SECTION:FINAL_SUMMARY:END -->
