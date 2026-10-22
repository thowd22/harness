//! Session-scoped approvals.

use std::path::Path;
use std::sync::RwLock;

use crate::rules::{CommandPattern, PathGlob};

#[derive(Debug, Clone)]
enum Grant {
    Command(CommandPattern),
    Path(PathGlob),
}

/// Approvals that last for the session. Shared between the policy engine
/// (which consults them for `ask` decisions) and the approval broker (which
/// adds them). They never override a deny.
#[derive(Debug, Default)]
pub struct SessionGrants {
    grants: RwLock<Vec<Grant>>,
}

impl SessionGrants {
    /// Allow commands matching `pattern` for the rest of the session.
    pub fn allow_command(&self, pattern: &str) {
        self.write()
            .push(Grant::Command(CommandPattern::new(pattern)));
    }

    /// Allow edits to paths matching `pattern` (relative to `root`).
    pub fn allow_path(&self, pattern: &str, root: &Path) -> Result<(), globset::Error> {
        let g = PathGlob::new(pattern, None, root)?;
        self.write().push(Grant::Path(g));
        Ok(())
    }

    /// The first command grant matching any of `forms`.
    pub fn matching_command<S: AsRef<str>>(&self, forms: &[S]) -> Option<String> {
        self.read().iter().find_map(|g| match g {
            Grant::Command(p) if p.matches_any(forms) => Some(p.as_str().to_string()),
            _ => None,
        })
    }

    /// The first path grant matching `abs`.
    pub fn matching_path(&self, abs: &Path, _root: &Path) -> Option<String> {
        self.read().iter().find_map(|g| match g {
            Grant::Path(p) if p.matches(abs) => Some(p.as_str().to_string()),
            _ => None,
        })
    }

    /// Every grant as written, for display (`/approvals`).
    pub fn list(&self) -> Vec<String> {
        self.read()
            .iter()
            .map(|g| match g {
                Grant::Command(p) => format!("command: {}", p.as_str()),
                Grant::Path(p) => format!("path: {}", p.as_str()),
            })
            .collect()
    }

    /// Forget every grant.
    pub fn clear(&self) {
        self.write().clear();
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Vec<Grant>> {
        self.grants.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<Grant>> {
        self.grants.write().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_match_and_clear() {
        let g = SessionGrants::default();
        g.allow_command("cargo test*");
        g.allow_path("src/**", Path::new("/ws")).unwrap();
        assert_eq!(
            g.matching_command(&["cargo test -p x"]),
            Some("cargo test*".into())
        );
        assert_eq!(g.matching_command(&["cargo build"]), None);
        assert_eq!(
            g.matching_path(Path::new("/ws/src/a.rs"), Path::new("/ws")),
            Some("src/**".into())
        );
        assert_eq!(
            g.matching_path(Path::new("/ws/b.rs"), Path::new("/ws")),
            None
        );
        assert_eq!(g.list(), ["command: cargo test*", "path: src/**"]);
        g.clear();
        assert!(g.list().is_empty());
    }
}
