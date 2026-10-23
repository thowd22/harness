//! The sandbox launcher: apply Landlock to itself, then `exec` the command.
//!
//! Usage: `<launcher> [--write PATH]... [--abi N] -- PROGRAM [ARGS...]`, or
//! `<launcher> --probe` to print the Landlock status (`landlock=full`,
//! `landlock=partial` or `landlock=none <reason>`).
//!
//! `--write` paths get write access without device ioctls. The terminal
//! devices ([`super::TERMINAL_DEVICES`] and the terminal on the launcher's
//! stdio, if any) always get write access **and** device ioctls, so
//! `ioctl(TIOCGWINSZ)`, `tcgetattr` and friends work on `/dev/tty` and pty
//! slaves while ioctls on every other device stay denied (Landlock ABI 5+).

use std::ffi::OsString;
use std::path::PathBuf;

use super::WARNING_PREFIX;

/// Grants write access beneath the following path.
pub const WRITE_FLAG: &str = "--write";
/// Prints the Landlock status and exits.
pub const PROBE_FLAG: &str = "--probe";
/// Targets an older Landlock ABI than the default (5), as on an older
/// kernel. Only ever loosens the ruleset to what that ABI can express; used
/// to test how the sandbox degrades.
pub const ABI_FLAG: &str = "--abi";

/// The newest Landlock ABI the ruleset uses (5 added device ioctls).
const DEFAULT_ABI: i32 = 5;

/// Outcome of applying the ruleset. Off Linux only `None` is constructed.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum Applied {
    Full,
    Partial,
    None(String),
}

#[cfg(target_os = "linux")]
fn apply(writable: &[PathBuf], abi: i32) -> Applied {
    use landlock::{
        ABI, AccessFs, BitFlags, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    };
    let abi = ABI::from(abi.min(DEFAULT_ABI));
    let handled = AccessFs::from_write(abi);
    let write = handled & !BitFlags::from(AccessFs::IoctlDev);
    let terminals = terminal_paths();
    let rules = rules::path_rules(writable, write, abi)
        .into_iter()
        .chain(rules::path_rules(&terminals, handled, abi))
        .map(Ok::<_, landlock::RulesetError>);
    let result = Ruleset::default()
        .handle_access(handled)
        .and_then(|r| r.create())
        .and_then(|r| r.add_rules(rules))
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
fn apply(_: &[PathBuf], _: i32) -> Applied {
    Applied::None("Landlock is Linux-only".into())
}

/// [`super::TERMINAL_DEVICES`] plus the terminal open on stdin, stdout or
/// stderr (a console such as `/dev/tty1` is not beneath `/dev/pts`).
#[cfg(target_os = "linux")]
fn terminal_paths() -> Vec<PathBuf> {
    use std::os::fd::AsFd;
    let mut paths: Vec<PathBuf> = super::TERMINAL_DEVICES.iter().map(PathBuf::from).collect();
    let stdio = [
        std::io::stdin().as_fd().try_clone_to_owned(),
        std::io::stdout().as_fd().try_clone_to_owned(),
        std::io::stderr().as_fd().try_clone_to_owned(),
    ];
    for fd in stdio.into_iter().flatten() {
        if nix::unistd::isatty(&fd).unwrap_or(false)
            && let Ok(p) = nix::unistd::ttyname(&fd)
            && !paths.contains(&p)
        {
            paths.push(p);
        }
    }
    paths
}

/// Building Landlock rules without opening the paths for real.
#[cfg(target_os = "linux")]
pub(crate) mod rules {
    use std::os::fd::OwnedFd;
    use std::path::{Path, PathBuf};

    use landlock::{ABI, AccessFs, BitFlags, PathBeneath};
    use nix::fcntl::OFlag;
    use nix::sys::stat::{Mode, SFlag};

    /// Opens `path` with `O_PATH`, which never opens the file itself (no
    /// tty `open`, no `ENXIO` for `/dev/tty` without a controlling
    /// terminal).
    ///
    /// `landlock::PathFd` relies on std's `custom_flags`, which drops
    /// `O_PATH` on musl (its `O_ACCMODE` includes `O_SEARCH`): the static
    /// release binary then opened `/dev/tty` for real, failed without a
    /// controlling terminal and silently skipped the rule.
    pub(crate) fn open_path(path: &Path) -> nix::Result<OwnedFd> {
        nix::fcntl::open(path, OFlag::O_PATH | OFlag::O_CLOEXEC, Mode::empty())
    }

    /// One rule per existing path: `access` beneath directories, and the
    /// file subset of `access` on anything else. Missing paths are skipped.
    pub(crate) fn path_rules(
        paths: &[PathBuf],
        access: BitFlags<AccessFs>,
        abi: ABI,
    ) -> Vec<PathBeneath<OwnedFd>> {
        paths
            .iter()
            .filter_map(|p| {
                let fd = open_path(p).ok()?;
                let dir = nix::sys::stat::fstat(&fd)
                    .map(|st| {
                        SFlag::from_bits_truncate(st.st_mode) & SFlag::S_IFMT == SFlag::S_IFDIR
                    })
                    .unwrap_or(true);
                let allowed = if dir {
                    access
                } else {
                    access & AccessFs::from_file(abi)
                };
                Some(PathBeneath::new(fd, allowed))
            })
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn open_path_uses_o_path_and_works_without_opening_devices() {
            // O_PATH never opens the tty, so it succeeds even without a
            // controlling terminal; the fd reports O_PATH in its flags.
            for p in ["/dev/tty", "/dev/null", "/tmp"] {
                let fd = open_path(Path::new(p)).unwrap();
                let flags = nix::fcntl::fcntl(&fd, nix::fcntl::FcntlArg::F_GETFL).unwrap();
                assert!(
                    OFlag::from_bits_truncate(flags).contains(OFlag::O_PATH),
                    "{p}: {flags:o}"
                );
            }
        }

        #[test]
        fn rules_skip_missing_paths_and_keep_device_rules() {
            let paths = [
                PathBuf::from("/dev/tty"),
                PathBuf::from("/nonexistent/harness-test"),
                PathBuf::from("/tmp"),
            ];
            let n = path_rules(&paths, AccessFs::from_write(ABI::V5), ABI::V5).len();
            assert_eq!(n, 2);
        }
    }
}

/// Entry point for the launcher. `args` excludes the program name (and any
/// [`super::LAUNCHER_ARG`]). Returns an exit code only on failure: on
/// success the process is replaced by the command.
pub fn launcher_main(args: impl IntoIterator<Item = OsString>) -> i32 {
    let mut args = args.into_iter();
    let mut writable = Vec::new();
    let mut probe = false;
    let mut abi = DEFAULT_ABI;
    let mut command: Vec<OsString> = Vec::new();
    while let Some(a) = args.next() {
        match a.to_str() {
            Some(WRITE_FLAG) => match args.next() {
                Some(p) => writable.push(PathBuf::from(p)),
                None => return usage("--write needs a path"),
            },
            Some(ABI_FLAG) => match args.next().and_then(|n| n.to_str()?.parse::<i32>().ok()) {
                Some(n) if n >= 1 => abi = n,
                _ => return usage("--abi needs a number from 1"),
            },
            Some(PROBE_FLAG) => probe = true,
            Some("--") => {
                command.extend(args.by_ref());
                break;
            }
            _ => return usage(&format!("unexpected argument {a:?}")),
        }
    }
    let applied = apply(&writable, abi);
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
    eprintln!(
        "{WARNING_PREFIX} {msg}. Usage: [--write PATH]... [--abi N] -- PROGRAM [ARGS...] | --probe"
    );
    2
}
