//! Typed accessors over INI sections.
//!
//! [`SectionReader`] reads keys from one [`Section`] as strings, booleans,
//! numbers, comma lists, durations, `~`-expanded paths, enums and secrets.
//! It remembers which keys were read, so [`SectionReader::finish`] can report
//! every leftover key as an unknown-key error with its line number. Parse
//! errors are collected rather than returned so one pass reports all of them.
//!
//! The pure parsers ([`parse_bool`], [`parse_list`], [`parse_duration`],
//! [`expand_tilde`]) are public for callers that hold raw strings.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use super::document::{Entry, Section};
use super::error::{IniError, closest};
use crate::secrets::Secret;

/// A closed set of string values accepted by an INI key.
pub trait ConfigEnum: Sized + Copy {
    /// Accepted spellings, in documentation order.
    const VARIANTS: &'static [&'static str];
    /// Parses one accepted spelling.
    fn from_ini(s: &str) -> Option<Self>;
    /// The spelling used in `harness.ini`.
    fn as_ini(&self) -> &'static str;
}

/// Declares a [`ConfigEnum`] with serde serialization as its INI spelling.
macro_rules! config_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }
        impl $crate::ini::value::ConfigEnum for $name {
            const VARIANTS: &'static [&'static str] = &[$($text),+];
            fn from_ini(s: &str) -> Option<Self> {
                match s { $($text => Some(Self::$variant),)+ _ => None }
            }
            fn as_ini(&self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str($crate::ini::value::ConfigEnum::as_ini(self))
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str($crate::ini::value::ConfigEnum::as_ini(self))
            }
        }
    };
}
pub(crate) use config_enum;

/// Parses `true` or `false` (lowercase, as documented).
pub fn parse_bool(s: &str) -> Result<bool, String> {
    match s {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("expected `true` or `false`".into()),
    }
}

/// Splits a comma-separated list, trimming items and dropping empty ones.
pub fn parse_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Parses a duration: a bare number means seconds; suffixes `ms`, `s`, `m`
/// and `h` are accepted (`500ms`, `20`, `20s`, `5m`, `1h`, `1.5s`).
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let err =
        || format!("expected a duration such as `20`, `20s`, `500ms`, `5m` or `1h`, got `{s}`");
    let (num, scale) = if let Some(n) = s.strip_suffix("ms") {
        (n, 0.001)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60.0)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600.0)
    } else {
        (s, 1.0)
    };
    let n: f64 = num.trim().parse().map_err(|_| err())?;
    if !n.is_finite() || n < 0.0 {
        return Err(err());
    }
    Duration::try_from_secs_f64(n * scale).map_err(|_| err())
}

/// Expands a leading `~` or `~/` to `home`. Other paths are returned as-is.
/// `~user` forms are rejected because they are ambiguous.
pub fn expand_tilde(s: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    if s == "~" || s.starts_with("~/") {
        let home = home.ok_or("cannot expand `~`: home directory is unknown")?;
        Ok(match s.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => home.to_path_buf(),
        })
    } else if s.starts_with('~') {
        Err("`~user` paths are not supported; write the full path".into())
    } else {
        Ok(PathBuf::from(s))
    }
}

/// Reads typed values from one section, tracking which keys were consumed.
pub struct SectionReader<'a> {
    section: &'a Section,
    home: Option<&'a Path>,
    used: BTreeSet<&'a str>,
    errors: &'a mut Vec<IniError>,
}

impl<'a> SectionReader<'a> {
    /// Creates a reader. Errors are appended to `errors`; `home` is used for
    /// `~` expansion in [`SectionReader::path`].
    pub fn new(
        section: &'a Section,
        home: Option<&'a Path>,
        errors: &'a mut Vec<IniError>,
    ) -> Self {
        Self {
            section,
            home,
            used: BTreeSet::new(),
            errors,
        }
    }

    /// The section being read.
    pub fn section(&self) -> &'a Section {
        self.section
    }

    fn take(&mut self, key: &'a str) -> Option<&'a Entry> {
        self.used.insert(key);
        self.section.get(key)
    }

    /// Records an invalid-value error for `entry`.
    pub fn invalid(&mut self, entry: &Entry, message: impl Into<String>) {
        self.errors.push(IniError::InvalidValue {
            location: entry.location.clone(),
            section: self.section.name.clone(),
            key: entry.key.clone(),
            message: message.into(),
        });
    }

    fn typed<T>(
        &mut self,
        key: &'static str,
        parse: impl FnOnce(&str) -> Result<T, String>,
    ) -> Option<T> {
        let entry = self.take(key)?;
        match parse(&entry.value) {
            Ok(v) => Some(v),
            Err(msg) => {
                self.invalid(entry, msg);
                None
            }
        }
    }

    /// Reads a string value as written.
    pub fn string(&mut self, key: &'static str) -> Option<String> {
        self.take(key).map(|e| e.value.clone())
    }

    /// Reads a non-empty string value.
    pub fn nonempty_string(&mut self, key: &'static str) -> Option<String> {
        self.typed(key, |s| {
            if s.is_empty() {
                Err("value must not be empty".into())
            } else {
                Ok(s.to_owned())
            }
        })
    }

    /// Reads a secret. Empty values count as absent. The value is never
    /// included in error messages.
    pub fn secret(&mut self, key: &'static str) -> Option<Secret> {
        let entry = self.take(key)?;
        (!entry.value.is_empty()).then(|| Secret::new(entry.value.clone()))
    }

    /// Reads `true` / `false`.
    pub fn bool(&mut self, key: &'static str) -> Option<bool> {
        self.typed(key, parse_bool)
    }

    /// Reads an unsigned integer of type `T`.
    pub fn uint<T: FromStr>(&mut self, key: &'static str) -> Option<T> {
        self.typed(key, |s| {
            s.parse::<T>()
                .map_err(|_| format!("expected a whole number in range, got `{s}`"))
        })
    }

    /// Reads a finite floating-point number.
    pub fn float(&mut self, key: &'static str) -> Option<f64> {
        self.typed(key, |s| match s.parse::<f64>() {
            Ok(v) if v.is_finite() => Ok(v),
            _ => Err(format!("expected a number, got `{s}`")),
        })
    }

    /// Reads a comma-separated list.
    pub fn list(&mut self, key: &'static str) -> Option<Vec<String>> {
        self.take(key).map(|e| parse_list(&e.value))
    }

    /// Reads a duration (see [`parse_duration`]).
    pub fn duration(&mut self, key: &'static str) -> Option<Duration> {
        self.typed(key, parse_duration)
    }

    /// Reads a path, expanding a leading `~`.
    pub fn path(&mut self, key: &'static str) -> Option<PathBuf> {
        let home = self.home;
        self.typed(key, |s| {
            if s.is_empty() {
                return Err("path must not be empty".into());
            }
            expand_tilde(s, home)
        })
    }

    /// Reads one of the values of a [`ConfigEnum`].
    pub fn enumerated<E: ConfigEnum>(&mut self, key: &'static str) -> Option<E> {
        self.typed(key, |s| {
            E::from_ini(s)
                .ok_or_else(|| format!("expected one of {}, got `{s}`", E::VARIANTS.join(" | ")))
        })
    }

    /// Marks every key in the section as read (for free-form sections whose
    /// keys are user-chosen names).
    pub fn mark_all_used(&mut self) {
        let section = self.section;
        self.used
            .extend(section.entries.iter().map(|e| e.key.as_str()));
    }

    /// Records a missing-required-key error at the section header.
    pub fn missing(&mut self, key: &str) {
        self.errors.push(IniError::MissingKey {
            location: self.section.location.clone(),
            section: self.section.name.clone(),
            key: key.to_owned(),
        });
    }

    /// Reports every key that was not read as unknown.
    pub fn finish(self) {
        let section = self.section;
        let known: Vec<&str> = self.used.iter().copied().collect();
        for entry in &section.entries {
            if !self.used.contains(entry.key.as_str()) {
                self.errors.push(IniError::UnknownKey {
                    location: entry.location.clone(),
                    section: section.name.clone(),
                    key: entry.key.clone(),
                    hint: closest(&entry.key, known.iter().copied()),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ini::document::parse;

    #[test]
    fn bools_are_strict() {
        assert_eq!(parse_bool("true"), Ok(true));
        assert_eq!(parse_bool("false"), Ok(false));
        assert!(parse_bool("yes").is_err());
        assert!(parse_bool("True").is_err());
    }

    #[test]
    fn lists_trim_and_drop_empties() {
        assert_eq!(parse_list(" gpu, bigmem ,,"), vec!["gpu", "bigmem"]);
        assert_eq!(
            parse_list("shift+enter, alt+enter"),
            vec!["shift+enter", "alt+enter"]
        );
        assert!(parse_list("").is_empty());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("20"), Ok(Duration::from_secs(20)));
        assert_eq!(parse_duration("20s"), Ok(Duration::from_secs(20)));
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("5m"), Ok(Duration::from_secs(300)));
        assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
        assert_eq!(parse_duration("1.5s"), Ok(Duration::from_millis(1500)));
        assert!(parse_duration("-1").is_err());
        assert!(parse_duration("soon").is_err());
    }

    #[test]
    fn tilde_expansion() {
        let home = Path::new("/home/me");
        assert_eq!(
            expand_tilde("~/x/y", Some(home)).unwrap(),
            PathBuf::from("/home/me/x/y")
        );
        assert_eq!(
            expand_tilde("~", Some(home)).unwrap(),
            PathBuf::from("/home/me")
        );
        assert_eq!(
            expand_tilde("/abs", Some(home)).unwrap(),
            PathBuf::from("/abs")
        );
        assert_eq!(expand_tilde("rel/~", None).unwrap(), PathBuf::from("rel/~"));
        assert!(expand_tilde("~bob/x", Some(home)).is_err());
        assert!(expand_tilde("~/x", None).is_err());
    }

    config_enum!(
        /// Test enum.
        Color { Red = "red", Blue = "blue" }
    );

    #[test]
    fn reader_types_and_unknown_keys() {
        let doc = parse(
            "[s]\nname = x\nflag = true\nn = 7\nf = 0.5\nl = a, b\nd = 2m\np = ~/q\nc = blue\ntypo = 1\nbad = maybe\n",
            None,
        )
        .unwrap();
        let mut errors = Vec::new();
        let home = PathBuf::from("/h");
        let mut r = SectionReader::new(&doc.sections[0], Some(&home), &mut errors);
        assert_eq!(r.string("name").as_deref(), Some("x"));
        assert_eq!(r.bool("flag"), Some(true));
        assert_eq!(r.uint::<u32>("n"), Some(7));
        assert_eq!(r.float("f"), Some(0.5));
        assert_eq!(r.list("l"), Some(vec!["a".into(), "b".into()]));
        assert_eq!(r.duration("d"), Some(Duration::from_secs(120)));
        assert_eq!(r.path("p"), Some(PathBuf::from("/h/q")));
        assert_eq!(r.enumerated::<Color>("c"), Some(Color::Blue));
        assert_eq!(r.bool("bad"), None);
        assert_eq!(r.string("missing"), None);
        r.finish();
        assert_eq!(errors.len(), 2);
        assert!(
            matches!(&errors[0], IniError::InvalidValue { key, location, .. } if key == "bad" && location.line == 11)
        );
        assert!(
            matches!(&errors[1], IniError::UnknownKey { key, location, .. } if key == "typo" && location.line == 10)
        );
    }
}
