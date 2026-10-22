//! Protected path matching.

use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use harness_tools::workspace::normalize;

/// Most paths one wildcard argument is expanded to when checking it.
pub const MAX_GLOB_EXPANSION: usize = 4096;

/// How a path is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Reading.
    Read,
    /// Creating, changing or deleting.
    Write,
}

/// A user glob from `paths.protected` (or a YAML overlay).
#[derive(Debug, Clone)]
pub struct PathGlob {
    pattern: String,
    matchers: Vec<GlobMatcher>,
}

impl PathGlob {
    /// Compile `pattern`. `~/` expands to `home`; absolute patterns and
    /// patterns starting with `**` are used as is; other relative patterns
    /// are relative to `root` (the workspace). A trailing `/**` also
    /// protects the directory itself.
    pub fn new(pattern: &str, home: Option<&Path>, root: &Path) -> Result<Self, globset::Error> {
        let pattern = pattern.trim();
        let expanded = if let Some(rest) = pattern.strip_prefix("~/") {
            match home {
                Some(h) => format!("{}/{rest}", h.display()),
                None => pattern.to_string(),
            }
        } else if pattern.starts_with('/') || pattern.starts_with("**") {
            pattern.to_string()
        } else {
            format!("{}/{pattern}", root.display())
        };
        let mut variants = vec![expanded.clone()];
        if let Some(base) = expanded.strip_suffix("/**")
            && !base.is_empty()
        {
            variants.push(base.to_string());
        }
        let matchers = variants
            .iter()
            .map(|v| {
                GlobBuilder::new(v)
                    .literal_separator(true)
                    .backslash_escape(true)
                    .build()
                    .map(|g| g.compile_matcher())
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            pattern: pattern.to_string(),
            matchers,
        })
    }

    /// The pattern as written.
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// True if `path` (absolute, normalised) matches.
    pub fn matches(&self, path: &Path) -> bool {
        self.matchers.iter().any(|m| m.is_match(path))
    }
}

/// Built-in protection that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltInHit {
    /// Beneath a protected root (harness.ini, data dir internals, trust store).
    Root(PathBuf),
    /// Inside a `.git/hooks` directory.
    GitHooks,
    /// Inside a project `.harness/` directory (write only).
    ProjectConfig,
}

/// Built-in protected locations.
#[derive(Debug, Clone, Default)]
pub struct BuiltInPaths {
    roots: Vec<PathBuf>,
}

impl BuiltInPaths {
    /// Protect each of `roots` and everything beneath it, for read and write.
    /// Roots are also resolved through symlinks.
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut v: Vec<PathBuf> = Vec::new();
        for r in roots {
            let n = normalize(&r);
            if let Ok(c) = std::fs::canonicalize(&n)
                && !v.contains(&c)
            {
                v.push(c);
            }
            if !v.contains(&n) {
                v.push(n);
            }
        }
        Self { roots: v }
    }

    /// The protected roots.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Check one absolute, normalised path.
    pub fn check(&self, path: &Path, access: Access) -> Option<BuiltInHit> {
        if let Some(r) = self.roots.iter().find(|r| path.starts_with(r)) {
            return Some(BuiltInHit::Root(r.clone()));
        }
        let names: Vec<&std::ffi::OsStr> = path
            .components()
            .filter_map(|c| match c {
                Component::Normal(n) => Some(n),
                _ => None,
            })
            .collect();
        if names.windows(2).any(|w| w[0] == ".git" && w[1] == "hooks") {
            return Some(BuiltInHit::GitHooks);
        }
        if access == Access::Write && names.iter().any(|n| *n == ".harness") {
            return Some(BuiltInHit::ProjectConfig);
        }
        None
    }
}

/// Expand `~` and `$HOME`/`${HOME}`/`$PWD`, join to `cwd` and normalise.
/// Returns the lexical path and, when it differs, the path with symlinks
/// resolved (as far as the path exists).
pub fn candidates(raw: &str, cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut s = raw.to_string();
    if let Some(h) = home {
        let h = h.display().to_string();
        if s == "~" {
            s = h.clone();
        } else if let Some(rest) = s.strip_prefix("~/") {
            s = format!("{h}/{rest}");
        }
        for var in ["${HOME}", "$HOME"] {
            s = s.replace(var, &h);
        }
    }
    let cwd_s = cwd.display().to_string();
    for var in ["${PWD}", "$PWD"] {
        s = s.replace(var, &cwd_s);
    }
    let lexical = normalize(&cwd.join(&s));
    let mut out = vec![lexical.clone()];
    if let Some(real) = resolve_existing(&lexical)
        && real != lexical
    {
        out.push(real);
    }
    out
}

/// Canonicalise the deepest existing ancestor and append the rest.
fn resolve_existing(path: &Path) -> Option<PathBuf> {
    let mut base = path.to_path_buf();
    let mut rest = Vec::new();
    loop {
        if let Ok(c) = std::fs::canonicalize(&base) {
            let mut out = c;
            for r in rest.iter().rev() {
                out.push(r);
            }
            return Some(out);
        }
        let name = base.file_name()?.to_os_string();
        rest.push(name);
        if !base.pop() {
            return None;
        }
    }
}

/// True if `s` contains glob characters the shell would expand.
pub fn has_wildcards(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Expand a wildcard path the way the shell would (dotfiles included, to
/// be safe), stopping after [`MAX_GLOB_EXPANSION`] results.
pub fn expand_wildcards(path: &Path) -> Vec<PathBuf> {
    let mut current = vec![PathBuf::new()];
    for comp in path.components() {
        let mut next = Vec::new();
        match comp {
            Component::Normal(name) if has_wildcards(&name.to_string_lossy()) => {
                let pat = name.to_string_lossy().replace("**", "*");
                let Ok(glob) = GlobBuilder::new(&pat).literal_separator(true).build() else {
                    return Vec::new();
                };
                let m = glob.compile_matcher();
                for dir in &current {
                    let Ok(rd) = std::fs::read_dir(dir) else {
                        continue;
                    };
                    for e in rd.flatten() {
                        if m.is_match(e.file_name()) {
                            next.push(dir.join(e.file_name()));
                            if next.len() >= MAX_GLOB_EXPANSION {
                                return next;
                            }
                        }
                    }
                }
            }
            other => next = current.iter().map(|d| d.join(other)).collect(),
        }
        current = next;
        if current.is_empty() {
            break;
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_expand_home_and_root() {
        let home = Path::new("/home/me");
        let root = Path::new("/ws");
        let g = PathGlob::new("~/.ssh/**", Some(home), root).unwrap();
        assert!(g.matches(Path::new("/home/me/.ssh/id_rsa")));
        assert!(g.matches(Path::new("/home/me/.ssh")));
        assert!(!g.matches(Path::new("/home/me/.sshx")));
        let env = PathGlob::new("**/.env*", Some(home), root).unwrap();
        assert!(env.matches(Path::new("/ws/app/.env.local")));
        assert!(env.matches(Path::new("/elsewhere/.env")));
        let rel = PathGlob::new("secrets/*.key", Some(home), root).unwrap();
        assert!(rel.matches(Path::new("/ws/secrets/a.key")));
        assert!(!rel.matches(Path::new("/ws/secrets/sub/a.key")));
    }

    #[test]
    fn builtins() {
        let b = BuiltInPaths::new([
            PathBuf::from("/data/harness"),
            PathBuf::from("/cfg/harness.ini"),
        ]);
        assert!(
            b.check(Path::new("/data/harness/trust/x"), Access::Read)
                .is_some()
        );
        assert!(
            b.check(Path::new("/cfg/harness.ini"), Access::Read)
                .is_some()
        );
        assert!(
            b.check(Path::new("/cfg/harness.ini.bak"), Access::Read)
                .is_none()
        );
        assert_eq!(
            b.check(Path::new("/ws/.git/hooks/pre-commit"), Access::Read),
            Some(BuiltInHit::GitHooks)
        );
        assert_eq!(
            b.check(Path::new("/ws/.harness/agents/a.yaml"), Access::Write),
            Some(BuiltInHit::ProjectConfig)
        );
        assert_eq!(
            b.check(Path::new("/ws/.harness/agents/a.yaml"), Access::Read),
            None
        );
    }

    #[test]
    fn candidates_resolve_symlinks_and_vars() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        let c = candidates("link/new.txt", dir.path(), None);
        assert_eq!(c[0], dir.path().join("link/new.txt"));
        assert_eq!(c[1], std::fs::canonicalize(&real).unwrap().join("new.txt"));
        let h = candidates(
            "$HOME/.ssh/../.ssh/x",
            Path::new("/w"),
            Some(Path::new("/home/me")),
        );
        assert_eq!(h[0], PathBuf::from("/home/me/.ssh/x"));
        assert_eq!(
            candidates("~", Path::new("/w"), Some(Path::new("/h")))[0],
            PathBuf::from("/h")
        );
    }

    #[test]
    fn wildcard_expansion() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".ssh")).unwrap();
        std::fs::write(dir.path().join(".ssh/id_rsa"), "k").unwrap();
        let got = expand_wildcards(&dir.path().join(".ss*/id_*"));
        assert_eq!(got, vec![dir.path().join(".ssh/id_rsa")]);
        assert!(has_wildcards("a*"));
        assert!(!has_wildcards("a/b"));
    }
}
