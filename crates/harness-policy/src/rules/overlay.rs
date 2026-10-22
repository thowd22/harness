//! Tighten-only policy overlays from agent and workflow YAML.
//!
//! "The repo can ask; only the user can grant." An overlay may add deny,
//! ask, outward and protected-path rules. Any key that would loosen policy
//! is rejected when the YAML is loaded, with an explanation.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Keys an overlay may set.
pub const OVERLAY_KEYS: &[&str] = &["bash.deny", "bash.ask", "paths.protected", "outward.ask"];

/// The `policy:` block of an agent or workflow YAML file.
///
/// Deserialises from a map whose values are a string or a list of strings:
///
/// ```yaml
/// policy:
///   bash.deny: ["git push*"]
///   paths.protected: secrets/**
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "BTreeMap<String, OneOrMany>")]
pub struct PolicyOverlay {
    /// Extra `bash.deny` patterns.
    pub bash_deny: Vec<String>,
    /// Extra `bash.ask` patterns.
    pub bash_ask: Vec<String>,
    /// Extra protected path globs.
    pub paths_protected: Vec<String>,
    /// Extra outward patterns.
    pub outward_ask: Vec<String>,
}

/// A string or a list of strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    /// One value.
    One(String),
    /// Several values.
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(s) => vec![s],
            Self::Many(v) => v,
        }
    }
}

/// Why an overlay was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OverlayError {
    /// The key would loosen policy.
    #[error(
        "`policy.{key}` would loosen policy, but agent and workflow YAML can only tighten it \
         (the repo can ask; only the user can grant). {hint}"
    )]
    Loosens {
        /// The offending key.
        key: String,
        /// How to achieve it legitimately.
        hint: String,
    },
    /// Unknown key.
    #[error("unknown policy key `{key}`; allowed keys are {}", OVERLAY_KEYS.join(", "))]
    UnknownKey {
        /// The key.
        key: String,
    },
    /// An empty pattern.
    #[error("`policy.{key}` contains an empty pattern")]
    EmptyPattern {
        /// The key.
        key: String,
    },
}

impl PolicyOverlay {
    /// Validate a raw key/value map.
    pub fn from_map(map: BTreeMap<String, Vec<String>>) -> Result<Self, OverlayError> {
        let mut o = Self::default();
        for (key, values) in map {
            if let Some(hint) = loosening_hint(&key) {
                return Err(OverlayError::Loosens { key, hint });
            }
            let slot = match key.as_str() {
                "bash.deny" => &mut o.bash_deny,
                "bash.ask" => &mut o.bash_ask,
                "paths.protected" => &mut o.paths_protected,
                "outward.ask" => &mut o.outward_ask,
                _ => return Err(OverlayError::UnknownKey { key }),
            };
            if values.iter().any(|v| v.trim().is_empty()) {
                return Err(OverlayError::EmptyPattern { key });
            }
            slot.extend(values.into_iter().map(|v| v.trim().to_string()));
        }
        Ok(o)
    }

    /// True if the overlay adds nothing.
    pub fn is_empty(&self) -> bool {
        self.bash_deny.is_empty()
            && self.bash_ask.is_empty()
            && self.paths_protected.is_empty()
            && self.outward_ask.is_empty()
    }
}

impl TryFrom<BTreeMap<String, OneOrMany>> for PolicyOverlay {
    type Error = OverlayError;

    fn try_from(map: BTreeMap<String, OneOrMany>) -> Result<Self, Self::Error> {
        Self::from_map(map.into_iter().map(|(k, v)| (k, v.into_vec())).collect())
    }
}

fn loosening_hint(key: &str) -> Option<String> {
    let ini = |k: &str| format!("Add the rule to `[policy] {k}` in your harness.ini instead.");
    match key {
        "bash.allow" => Some(ini("bash.allow")),
        "outward.allow" => Some(ini("outward.allow")),
        k if k.ends_with(".allow") || k == "allow" => Some(ini(k)),
        "paths.unprotected" | "paths.allow" | "paths.unprotect" => Some(
            "Protected paths can only be removed from `[policy] paths.protected` in harness.ini; built-in protected paths cannot be removed.".into(),
        ),
        "mode" | "autonomy" => Some(
            "Choose the autonomy mode with `[general] mode` in harness.ini or the command palette.".into(),
        ),
        "sandbox" | "sandbox.enabled" | "sandbox.network" => {
            Some("Sandbox settings live in `[sandbox]` in harness.ini.".into())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(s: &str) -> Result<PolicyOverlay, String> {
        serde_norway::from_str(s).map_err(|e| e.to_string())
    }

    #[test]
    fn accepts_tightening_keys() {
        let o = yaml("bash.deny: [\"git push*\"]\nbash.ask: npm *\npaths.protected: secrets/**\noutward.ask: [x]").unwrap();
        assert_eq!(o.bash_deny, ["git push*"]);
        assert_eq!(o.bash_ask, ["npm *"]);
        assert_eq!(o.paths_protected, ["secrets/**"]);
        assert_eq!(o.outward_ask, ["x"]);
    }

    #[test]
    fn rejects_loosening_with_explanation() {
        for key in [
            "bash.allow",
            "outward.allow",
            "paths.allow",
            "mode",
            "sandbox.enabled",
        ] {
            let e = yaml(&format!("{key}: x")).unwrap_err();
            assert!(e.contains("can only tighten"), "{key}: {e}");
            assert!(e.contains("harness.ini"), "{key}: {e}");
        }
    }

    #[test]
    fn rejects_unknown_and_empty() {
        assert!(
            yaml("bash.whatever: x")
                .unwrap_err()
                .contains("unknown policy key")
        );
        assert!(
            yaml("bash.deny: ['']")
                .unwrap_err()
                .contains("empty pattern")
        );
    }
}
