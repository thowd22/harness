//! The defaults fingerprint and the defaults changelog policy.
//!
//! Default changes (reasoning effort, verbosity, prompt wording) caused real
//! regressions elsewhere, so they must be deliberate and documented. The
//! *defaults fingerprint* covers every built-in prompt, agent and workflow
//! file ([`harness_config::builtins::fingerprint`]) and the `harness.ini`
//! defaults ([`harness_config::ini::Config::defaults`]). `CHANGELOG.md` has a
//! `## Defaults` section; [`check_changelog`] requires an entry there that
//! names the current fingerprint (`defaults-fingerprint: <12 hex>`), and a
//! test runs it in CI, so changing a default without a changelog entry
//! fails the build.

use sha2::{Digest, Sha256};

/// Heading of the defaults section in `CHANGELOG.md`.
pub const SECTION: &str = "## Defaults";
/// Marker that precedes the fingerprint in an entry.
pub const MARKER: &str = "defaults-fingerprint:";
/// Hex digits of the fingerprint written in the changelog.
pub const SHORT_LEN: usize = 12;

/// The current defaults fingerprint (full sha256 hex).
pub fn fingerprint() -> String {
    let mut cfg = harness_config::ini::Config::defaults();
    // Machine-specific; excluded from the hash anyway.
    cfg.general.data_dir = "/".into();
    let mut h = Sha256::new();
    h.update(harness_config::builtins::fingerprint().as_bytes());
    h.update([0]);
    h.update(harness_config::effective::config_hash(&cfg, None).as_bytes());
    hex::encode(h.finalize())
}

/// The fingerprint as written in the changelog.
pub fn short_fingerprint() -> String {
    fingerprint()[..SHORT_LEN].to_owned()
}

/// Checks that `changelog` has a [`SECTION`] with an entry naming the
/// current fingerprint. The error says exactly what to add.
pub fn check_changelog(changelog: &str) -> Result<(), String> {
    let short = short_fingerprint();
    // The section heading is a line of its own (the intro may mention it).
    let start = changelog.match_indices(SECTION).map(|(i, _)| i).find(|&i| {
        (i == 0 || changelog[..i].ends_with('\n'))
            && changelog[i + SECTION.len()..]
                .chars()
                .next()
                .is_none_or(|c| c == '\n' || c == '\r')
    });
    let Some(start) = start else {
        return Err(format!(
            "CHANGELOG.md has no `{SECTION}` section; add one with an entry ending in `{MARKER} {short}`"
        ));
    };
    let body = &changelog[start + SECTION.len()..];
    let end = body.find("\n## ").unwrap_or(body.len());
    let section = &body[..end];
    if section.contains(&format!("{MARKER} {short}")) {
        Ok(())
    } else {
        Err(format!(
            "built-in prompts, agents, workflows or harness.ini defaults changed: add an entry to the \
             `{SECTION}` section of CHANGELOG.md describing the change and ending in `{MARKER} {short}` \
             (see docs/defaults.md)"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable() {
        assert_eq!(fingerprint(), fingerprint());
        assert_eq!(short_fingerprint().len(), SHORT_LEN);
    }

    #[test]
    fn check_requires_the_current_fingerprint_in_the_defaults_section() {
        let fp = short_fingerprint();
        assert!(
            check_changelog("# Changelog\n")
                .unwrap_err()
                .contains("no `## Defaults`")
        );
        let stale = format!("{SECTION}\n- old change. {MARKER} 000000000000\n");
        assert!(check_changelog(&stale).unwrap_err().contains(&fp));
        let ok = format!("# Changelog\n\n{SECTION}\n\n- New default. {MARKER} {fp}\n\n## 0.1.0\n");
        assert!(check_changelog(&ok).is_ok());
        let mentioned = format!(
            "# Changelog\n\nThe `{SECTION}` section at the end.\n\n## Unreleased\n\n{SECTION}\n\n- New. {MARKER} {fp}\n"
        );
        assert!(
            check_changelog(&mentioned).is_ok(),
            "a mention in the intro is not the section"
        );
        let elsewhere = format!("{SECTION}\n- x\n\n## 0.1.0\n- {MARKER} {fp}\n");
        assert!(
            check_changelog(&elsewhere).is_err(),
            "must be in the Defaults section"
        );
    }
}
