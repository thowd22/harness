//! `harness remote-agent --stdio`: the remote helper (see `docs/remote.md`).
//!
//! Dispatched before harness.ini, the data directory or logging are touched:
//! on a remote host there is usually no harness configuration at all, and
//! stdout belongs to the JSON-RPC protocol.

use std::process::ExitCode;

/// Run the helper with the arguments after `remote-agent`.
pub fn run(args: &[String]) -> ExitCode {
    let rc = harness_remote::helper::main(args.to_vec());
    ExitCode::from(u8::try_from(rc).unwrap_or(1))
}
