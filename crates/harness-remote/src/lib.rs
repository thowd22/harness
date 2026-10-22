//! `harness-remote`: SSH transport, remote helper protocol, remote `Workspace`, sync and host probing.
//!
//! Local brain, remote hands: the model loop, credentials, memory and the
//! event log stay on the local machine; a remote host only receives file and
//! process operations through the helper (`harness remote-agent --stdio`).
//!
//! - [`hosts`]: hosts declared in `harness.ini` and the `/remote` view.
//! - [`ssh`]: system OpenSSH with ControlMaster multiplexing.
//! - [`protocol`]: the versioned JSON-RPC protocol.
//! - [`wire`]: framing, binary payloads and negotiated zstd compression.
//! - [`helper`]: the helper side (runs on the remote host).
//! - [`client`]: connection lifecycle, reconnect and job reattach.
//! - [`workspace`]: `Workspace`/`Exec` over the helper.
//! - [`install`]: on-demand upload and upgrade of the helper.
//! - [`shell`]: POSIX shell fallback when upload is not allowed.
//! - [`sync`]: worktree push, change pull, conflict-safe apply, cleanup.
//! - [`node`]: one node's tools on a host (connect, push, tools, pull, apply).
//! - [`probe`]: resources and health.
//! - [`link`]: link throughput, per-host SSH compression, rsync zstd.
//! - [`cache`]: content-hash cache that turns repeat transfers into hashes.
//! - [`audit`]: per-operation audit events and the secret guard.
//! - [`simlink`]: a throttled in-memory link for protocol tests and benchmarks.
//!
//! See `docs/remote.md` and `backlog/docs/doc-6 - Remote-Execution-over-SSH.md`.

pub mod audit;
pub mod cache;
pub mod client;
pub mod helper;
pub mod hosts;
pub mod install;
pub mod link;
pub mod node;
pub mod probe;
pub mod protocol;
pub mod shell;
pub mod simlink;
pub mod ssh;
pub mod sync;
pub mod wire;
pub mod workspace;
