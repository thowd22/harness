//! Locating, reading and validating `harness.ini`, plus the
//! `harness config check` entry point.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::config::Config;
use super::document::parse;
use super::error::{IniError, IniErrors};
use crate::paths::{ConfigLocation, config_location, home_dir};

/// Loads the config from `$HARNESS_CONFIG` or the XDG default path.
///
/// A missing file at the default path yields built-in defaults; a missing
/// file named by `$HARNESS_CONFIG` is an error.
pub fn load() -> Result<Config, IniErrors> {
    load_location(&config_location(|k| std::env::var_os(k)))
}

/// Loads the config from an already-resolved location.
pub fn load_location(location: &ConfigLocation) -> Result<Config, IniErrors> {
    let path = location.path();
    if !location.is_explicit() && !path.exists() {
        tracing::debug!(path = %path.display(), "no harness.ini; using defaults");
        return Ok(Config::defaults());
    }
    load_file(path)
}

/// Loads and validates the file at `path`, which must exist.
///
/// Refuses files that contain inline `api_key` secrets but are readable by
/// group or others.
pub fn load_file(path: &Path) -> Result<Config, IniErrors> {
    let text = std::fs::read_to_string(path).map_err(|source| IniError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let config = load_str(&text, Some(path))?;
    if config.has_inline_secrets()
        && let Some(mode) = crate::secrets::insecure_mode(path).map_err(|source| IniError::Io {
            path: path.to_path_buf(),
            source,
        })?
    {
        return Err(IniError::InsecurePermissions {
            path: path.to_path_buf(),
            mode,
        }
        .into());
    }
    tracing::debug!(path = %path.display(), "loaded harness.ini");
    Ok(config)
}

/// Parses and validates INI `text` over the built-in defaults. `file` only
/// labels error locations. No permission check is done.
pub fn load_str(text: &str, file: Option<&Path>) -> Result<Config, IniErrors> {
    let doc = parse(text, file).map_err(IniErrors)?;
    let home = home_dir();
    Config::from_document(&doc, Config::defaults(), home.as_deref()).map_err(IniErrors)
}

/// Implements `harness config check [path]`: validates the file and writes a
/// report. Returns the process exit code: 0 when valid, 1 on any error.
///
/// With `path = None` the normal lookup (`$HARNESS_CONFIG`, then XDG) is used.
pub fn run_check(path: Option<&Path>, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let location = match path {
        Some(p) => ConfigLocation::Explicit(p.to_path_buf()),
        None => config_location(|k| std::env::var_os(k)),
    };
    let shown: PathBuf = location.path().to_path_buf();
    if !location.is_explicit() && !shown.exists() {
        let _ = writeln!(
            out,
            "{}: not found; built-in defaults apply",
            shown.display()
        );
        return 0;
    }
    match load_location(&location) {
        Ok(_) => {
            let _ = writeln!(out, "{}: ok", shown.display());
            0
        }
        Err(errors) => {
            let _ = writeln!(err, "{errors}");
            let n = errors.errors().len();
            let _ = writeln!(
                err,
                "{}: {n} error{}",
                shown.display(),
                if n == 1 { "" } else { "s" }
            );
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str, mode: u32) -> PathBuf {
        let p = dir.join("harness.ini");
        std::fs::write(&p, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = mode;
        p
    }

    #[test]
    fn missing_default_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let loc = ConfigLocation::Default(dir.path().join("nope.ini"));
        let c = load_location(&loc).unwrap();
        assert!(c.source.is_none());
        assert_eq!(c.general.default_model, "sonnet");
    }

    #[test]
    fn missing_explicit_file_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let loc = ConfigLocation::Explicit(dir.path().join("nope.ini"));
        let e = load_location(&loc).unwrap_err();
        assert!(matches!(e.errors()[0], IniError::Io { .. }));
    }

    #[test]
    fn loads_file_and_records_source() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "[general]\ndefault_model = opus\n", 0o600);
        let c = load_file(&p).unwrap();
        assert_eq!(c.general.default_model, "opus");
        assert_eq!(c.source.as_deref(), Some(p.as_path()));
    }

    #[test]
    fn check_reports_ok_and_errors_with_exit_codes() {
        let dir = tempfile::tempdir().unwrap();
        let good = write(dir.path(), "[ui]\nmouse = false\n", 0o600);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run_check(Some(&good), &mut out, &mut err), 0);
        assert!(String::from_utf8(out).unwrap().ends_with(": ok\n"));

        let bad = write(dir.path(), "[ui]\nmose = false\n[nope]\n", 0o600);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run_check(Some(&bad), &mut out, &mut err), 1);
        let err = String::from_utf8(err).unwrap();
        assert!(
            err.contains("harness.ini:2: unknown key `mose` in [ui] (did you mean `mouse`?)"),
            "{err}"
        );
        assert!(
            err.contains("harness.ini:3: unknown section [nope]"),
            "{err}"
        );
        assert!(err.contains("2 errors"));
    }
}
