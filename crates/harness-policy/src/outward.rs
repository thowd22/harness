//! Outward-action guard (push, publish, PR creation and similar).
//!
//! Outward actions change things other people see. They always ask, in
//! every autonomy mode, unless the user explicitly allows them in
//! `harness.ini`: a `[policy] bash.allow` pattern counts as explicit when
//! its fixed prefix (the text before the first wildcard) is itself an
//! outward command. `git push origin feature/*` allows those pushes;
//! `git *` or `*` does not.
//!
//! Patterns are matched against every *canonical form* of each simple
//! command, so aliased spellings are caught too: `/usr/bin/git push`,
//! `git -C repo -c k=v --no-pager push`, `gh pr new`, `npm --tag x publish`,
//! `sudo git push`, `bash -c 'git push'` (the last two through
//! [`crate::shell`] unwrapping).

use crate::shell::SimpleCommand;

/// Outward patterns that apply even when `outward.ask` is empty.
/// `[policy] outward.ask` adds to this list.
pub const DEFAULT_OUTWARD: &[&str] = &[
    "git push*",
    "git send-email*",
    "git svn dcommit*",
    "gh pr create*",
    "gh pr merge*",
    "gh pr review*",
    "gh pr comment*",
    "gh issue create*",
    "gh issue comment*",
    "gh release create*",
    "gh repo create*",
    "gh gist create*",
    "gh api*",
    "glab mr create*",
    "glab mr merge*",
    "glab release create*",
    "hub push*",
    "hub pull-request*",
    "cargo publish*",
    "cargo yank*",
    "cargo owner*",
    "npm publish*",
    "npm unpublish*",
    "npm deprecate*",
    "yarn publish*",
    "yarn npm publish*",
    "pnpm publish*",
    "bun publish*",
    "twine upload*",
    "poetry publish*",
    "uv publish*",
    "flit publish*",
    "hatch publish*",
    "gem push*",
    "gem yank*",
    "docker push*",
    "podman push*",
    "buildah push*",
    "helm push*",
    "dotnet nuget push*",
    "mvn deploy*",
    "gradle publish*",
    "./gradlew publish*",
    "terraform apply*",
    "kubectl apply*",
];

/// Global options that take a separate value, per program. These are
/// skipped so `git -C dir push` reads as `git push`.
fn value_options(program: &str) -> &'static [&'static str] {
    match program {
        "git" => &[
            "-C",
            "-c",
            "--git-dir",
            "--work-tree",
            "--namespace",
            "--exec-path",
            "--config-env",
            "--super-prefix",
            "--list-cmds",
        ],
        "gh" => &["-R", "--repo", "--hostname"],
        "glab" => &["-R", "--repo"],
        "cargo" => &["--config", "-Z", "--color", "--manifest-path"],
        "npm" | "pnpm" | "yarn" | "bun" => &[
            "--registry",
            "--tag",
            "--access",
            "--otp",
            "--prefix",
            "-C",
            "--cwd",
            "--dir",
            "--userconfig",
            "-w",
            "--workspace",
            "--filter",
        ],
        "docker" | "podman" => &[
            "-H",
            "--host",
            "--context",
            "-c",
            "--config",
            "-l",
            "--log-level",
        ],
        "helm" => &["--kube-context", "-n", "--namespace"],
        "kubectl" => &[
            "-n",
            "--namespace",
            "--context",
            "--kubeconfig",
            "-s",
            "--server",
        ],
        "terraform" => &["-chdir"],
        _ => &[],
    }
}

/// Sub-command aliases that mean the same outward action.
const SUBCOMMAND_ALIASES: &[(&str, &str, &str)] = &[
    ("gh", "pr new", "pr create"),
    ("gh", "issue new", "issue create"),
    ("gh", "release new", "release create"),
    ("glab", "mr new", "mr create"),
    ("npm", "pub", "publish"),
    ("npm", "unpub", "unpublish"),
    ("docker", "image push", "push"),
    ("podman", "image push", "push"),
];

/// Every canonical spelling of `cmd` that outward (and other command)
/// patterns are matched against. The first entry is the line as written.
pub fn canonical_forms(cmd: &SimpleCommand) -> Vec<String> {
    let mut out = cmd.forms();
    let Some(name) = cmd.program_name() else {
        return out;
    };
    let args: Vec<&str> = cmd.args().iter().map(|w| w.text.as_str()).collect();
    let stripped = strip_global_options(name, &args);
    let mut line = std::iter::once(name)
        .chain(stripped.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    push_unique(&mut out, line.clone());
    for (prog, alias, canonical) in SUBCOMMAND_ALIASES {
        if *prog != name {
            continue;
        }
        let prefix = format!("{prog} {alias}");
        if line == prefix || line.starts_with(&format!("{prefix} ")) {
            line = format!("{prog} {canonical}{}", &line[prefix.len()..]);
            push_unique(&mut out, line.clone());
        }
    }
    out
}

/// Drop leading options (and their values) between the program and its
/// sub-command, e.g. `cargo +nightly publish` or `git --no-pager push`.
fn strip_global_options<'a>(program: &str, args: &[&'a str]) -> Vec<&'a str> {
    let takes_value = value_options(program);
    let mut i = 0;
    while let Some(a) = args.get(i) {
        let is_opt =
            (a.starts_with('-') && *a != "-") || (program == "cargo" && a.starts_with('+'));
        if !is_opt {
            break;
        }
        if *a == "--" {
            i += 1;
            break;
        }
        i += if takes_value.contains(a) { 2 } else { 1 };
    }
    args.get(i..).map(<[_]>::to_vec).unwrap_or_default()
}

fn push_unique(out: &mut Vec<String>, s: String) {
    if !out.contains(&s) {
        out.push(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::parse;

    fn forms(src: &str) -> Vec<String> {
        canonical_forms(&parse(src).unwrap().commands[0])
    }

    #[test]
    fn strips_global_options_and_paths() {
        assert!(
            forms("git -C repo -c a=b --no-pager push origin").contains(&"git push origin".into())
        );
        assert!(forms("/usr/bin/git push").contains(&"git push".into()));
        assert!(forms("cargo +nightly publish").contains(&"cargo publish".into()));
        assert!(forms("npm --registry https://r publish").contains(&"npm publish".into()));
    }

    #[test]
    fn aliases_are_canonicalised() {
        assert!(forms("gh pr new --fill").contains(&"gh pr create --fill".into()));
        assert!(forms("gh -R o/r pr new").contains(&"gh pr create".into()));
        assert!(forms("docker image push x").contains(&"docker push x".into()));
        assert!(forms("npm pub").contains(&"npm publish".into()));
    }

    #[test]
    fn first_form_is_as_written() {
        assert_eq!(forms("git -C x status")[0], "git -C x status");
    }
}
