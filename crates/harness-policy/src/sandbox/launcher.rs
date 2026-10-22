//! The sandbox launcher: apply Landlock to itself, then `exec` the command.
//!
//! Usage: `<launcher> [--write PATH]... -- PROGRAM [ARGS...]`, or
//! `<launcher> --probe` to print the Landlock status (`landlock=full`,
//! `landlock=partial` or `landlock=none <reason>`).

use std::ffi::OsString;
use std::path::PathBuf;

use super::WARNING_PREFIX;

/// Grants write access beneath the following path.
pub const WRITE_FLAG: &str = "--write";
/// Prints the Landlock status and exits.
pub const PROBE_FLAG: &str = "--probe";

/// Outcome of applying the ruleset. Off Linux only `None` is constructed.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum Applied {
    Full,
    Partial,
    None(String),
}

#[cfg(target_os = "linux")]
fn apply(writable: &[PathBuf]) -> Applied {
    use landlock::{
        ABI, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, path_beneath_rules,
    };
    let abi = ABI::V5;
    let result = Ruleset::default()
        .handle_access(AccessFs::from_write(abi))
        .and_then(|r| r.create())
        .and_then(|r| r.add_rules(path_beneath_rules(writable, AccessFs::from_write(abi))))
        .and_then(|r| r.restrict_self());
    match result {
        Ok(s) => match s.ruleset {
            RulesetStatus::FullyEnforced => Applied::Full,
            RulesetStatus::PartiallyEnforced => Applied::Partial,
            RulesetStatus::NotEnforced => Applied::None("kernel without Landlock support".into()),
        },
        Err(e) => Applied::None(e.to_string()),
    }
}

#[cfg(not(target_os = "linux"))]
fn apply(_: &[PathBuf]) -> Applied {
    Applied::None("Landlock is Linux-only".into())
}

/// Entry point for the launcher. `args` excludes the program name (and any
/// [`super::LAUNCHER_ARG`]). Returns an exit code only on failure: on
/// success the process is replaced by the command.
pub fn launcher_main(args: impl IntoIterator<Item = OsString>) -> i32 {
    let mut args = args.into_iter();
    let mut writable = Vec::new();
    let mut probe = false;
    let mut command: Vec<OsString> = Vec::new();
    while let Some(a) = args.next() {
        match a.to_str() {
            Some(WRITE_FLAG) => match args.next() {
                Some(p) => writable.push(PathBuf::from(p)),
                None => return usage("--write needs a path"),
            },
            Some(PROBE_FLAG) => probe = true,
            Some("--") => {
                command.extend(args.by_ref());
                break;
            }
            _ => return usage(&format!("unexpected argument {a:?}")),
        }
    }
    let applied = apply(&writable);
    if probe {
        println!(
            "{}",
            match applied {
                Applied::Full => "landlock=full".to_string(),
                Applied::Partial => "landlock=partial".into(),
                Applied::None(r) => format!("landlock=none {r}"),
            }
        );
        return 0;
    }
    match applied {
        Applied::Full => {}
        Applied::Partial => {
            eprintln!("{WARNING_PREFIX} Landlock is only partially enforced on this kernel.");
        }
        Applied::None(reason) => {
            eprintln!(
                "{WARNING_PREFIX} Landlock unavailable ({reason}); writes outside the workspace are not blocked."
            );
        }
    }
    let Some((program, rest)) = command.split_first() else {
        return usage("missing command after --");
    };
    exec(program, rest)
}

fn exec(program: &OsString, args: &[OsString]) -> i32 {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(program).args(args).exec();
    eprintln!("{WARNING_PREFIX} cannot run {program:?}: {err}");
    127
}

fn usage(msg: &str) -> i32 {
    eprintln!("{WARNING_PREFIX} {msg}. Usage: [--write PATH]... -- PROGRAM [ARGS...] | --probe");
    2
}
