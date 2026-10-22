//! Standalone remote helper binary (`harness-remote-agent --stdio`).
//!
//! The main `harness` binary serves the same helper as
//! `harness remote-agent --stdio`; this small binary exists for tests and
//! for hosts where only the helper is wanted.

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("remote-agent") {
        args.remove(0);
    }
    std::process::exit(harness_remote::helper::main(args));
}
