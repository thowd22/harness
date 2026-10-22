//! Config and data directory layout (`data_dir`, runs, logs, trust store).
//!
//! Config file lookup: `$HARNESS_CONFIG`, else
//! `$XDG_CONFIG_HOME/harness/harness.ini`, else `~/.config/harness/harness.ini`.
//!
//! Data directory (`[general] data_dir`, default `$XDG_DATA_HOME/harness`,
//! else `~/.local/share/harness`):
//!
//! ```text
//! <data_dir>/
//!   layout-version    integer layout version (see LAYOUT_VERSION)
//!   runs/             one directory per run: events, artifacts, spill files
//!   sessions/         persisted chat sessions for resume
//!   checkpoints/      file snapshots for /undo
//!   trust/            project trust store
//!   memory/           memory drive (unless [memory] drive points elsewhere)
//!   remote-cache/     remote helper binaries and per-host sync state
//!   logs/             daily-rotated log files
//!   model-lists/      cached live model lists (created on first refresh)
//! ```
//!
//! [`DataDir::open`] creates missing directories (mode `0700` on Unix) and
//! migrates older layouts in place. All internal directories, the version
//! file and `harness.ini` itself are reported by
//! [`DataDir::protected_paths`] so the policy engine can deny agent tools
//! access to them.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use crate::ini::Config;

/// Environment variable naming an explicit config file.
pub const CONFIG_ENV: &str = "HARNESS_CONFIG";

/// Current data directory layout version.
pub const LAYOUT_VERSION: u32 = 1;

/// Name of the layout version file inside the data directory.
pub const LAYOUT_VERSION_FILE: &str = "layout-version";

/// The user's home directory, if known.
pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Where the config file is expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigLocation {
    /// Named explicitly (`$HARNESS_CONFIG` or a CLI argument); must exist.
    Explicit(PathBuf),
    /// The XDG default; may be absent, in which case defaults apply.
    Default(PathBuf),
}

impl ConfigLocation {
    /// The file path.
    pub fn path(&self) -> &Path {
        match self {
            ConfigLocation::Explicit(p) | ConfigLocation::Default(p) => p,
        }
    }

    /// True for [`ConfigLocation::Explicit`].
    pub fn is_explicit(&self) -> bool {
        matches!(self, ConfigLocation::Explicit(_))
    }
}

fn nonempty_abs(v: Option<OsString>) -> Option<PathBuf> {
    v.filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// Resolves the config file location using `env` for variable lookup.
pub fn config_location(env: impl Fn(&str) -> Option<OsString>) -> ConfigLocation {
    config_location_with(env, home_dir().as_deref())
}

/// Like [`config_location`] with an explicit home directory.
pub fn config_location_with(
    env: impl Fn(&str) -> Option<OsString>,
    home: Option<&Path>,
) -> ConfigLocation {
    if let Some(p) = env(CONFIG_ENV).filter(|v| !v.is_empty()) {
        return ConfigLocation::Explicit(PathBuf::from(p));
    }
    let base = nonempty_abs(env("XDG_CONFIG_HOME"))
        .or_else(|| home.map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    ConfigLocation::Default(base.join("harness").join("harness.ini"))
}

/// Default `data_dir` from the process environment.
pub fn default_data_dir() -> PathBuf {
    default_data_dir_with(|k| std::env::var_os(k), home_dir().as_deref())
}

/// Default `data_dir`: `$XDG_DATA_HOME/harness`, else `~/.local/share/harness`.
pub fn default_data_dir_with(
    env: impl Fn(&str) -> Option<OsString>,
    home: Option<&Path>,
) -> PathBuf {
    nonempty_abs(env("XDG_DATA_HOME"))
        .or_else(|| home.map(|h| h.join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("harness")
}

/// Errors from opening or migrating the data directory.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LayoutError {
    /// Filesystem error.
    #[error("{}: {source}", .path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The version file is not a number.
    #[error("{}: not a layout version number", .0.display())]
    BadVersionFile(PathBuf),
    /// The directory was written by a newer harness.
    #[error("{} uses layout version {found}, but this harness supports up to {supported}; upgrade harness", .path.display())]
    TooNew {
        /// Data directory.
        path: PathBuf,
        /// Version found on disk.
        found: u32,
        /// Highest supported version.
        supported: u32,
    },
    /// No migration step starts at this version.
    #[error("no migration from layout version {0}")]
    NoMigration(u32),
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> LayoutError + '_ {
    move |source| LayoutError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// One step that upgrades a data directory from `from` to `from + 1`.
#[derive(Clone, Copy)]
pub struct Migration {
    /// Version this step upgrades from.
    pub from: u32,
    /// Human-readable summary (logged when applied).
    pub description: &'static str,
    /// Applies the step to the data directory root.
    pub apply: fn(&Path) -> io::Result<()>,
}

/// Built-in migrations, in order.
///
/// Version 0 is a data directory created before the layout was versioned
/// (it may already contain `logs/`). Step 0 → 1 creates the standard
/// directories and keeps everything already present.
pub const MIGRATIONS: &[Migration] = &[Migration {
    from: 0,
    description: "create the standard data directory layout",
    apply: migrate_0_to_1,
}];

fn migrate_0_to_1(root: &Path) -> io::Result<()> {
    for dir in DataDir::new(root).internal_dirs() {
        create_private_dir(&dir)?;
    }
    Ok(())
}

/// Creates `dir` and its parents; new directories get mode `0700` on Unix.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// Reads the layout version; `Ok(None)` when the file does not exist.
pub fn read_layout_version(root: &Path) -> Result<Option<u32>, LayoutError> {
    let file = root.join(LAYOUT_VERSION_FILE);
    match std::fs::read_to_string(&file) {
        Ok(s) => s
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| LayoutError::BadVersionFile(file)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(&file)(e)),
    }
}

/// Writes the layout version atomically (temp file + rename).
pub fn write_layout_version(root: &Path, version: u32) -> Result<(), LayoutError> {
    let file = root.join(LAYOUT_VERSION_FILE);
    let tmp = root.join(format!(".{LAYOUT_VERSION_FILE}.tmp"));
    std::fs::write(&tmp, format!("{version}\n")).map_err(io_err(&tmp))?;
    std::fs::rename(&tmp, &file).map_err(io_err(&file))
}

/// Brings the data directory at `root` to `target` by applying `migrations`
/// in sequence, writing the version file after each step so an interrupted
/// upgrade resumes where it stopped. A missing version file means version 0.
/// Returns the version before migration.
pub fn migrate(root: &Path, migrations: &[Migration], target: u32) -> Result<u32, LayoutError> {
    create_private_dir(root).map_err(io_err(root))?;
    let start = read_layout_version(root)?.unwrap_or(0);
    if start > target {
        return Err(LayoutError::TooNew {
            path: root.to_path_buf(),
            found: start,
            supported: target,
        });
    }
    let mut v = start;
    while v < target {
        let step = migrations
            .iter()
            .find(|m| m.from == v)
            .ok_or(LayoutError::NoMigration(v))?;
        tracing::info!(
            from = v,
            to = v + 1,
            step = step.description,
            "migrating data directory"
        );
        (step.apply)(root).map_err(io_err(root))?;
        v += 1;
        write_layout_version(root, v)?;
    }
    Ok(start)
}

/// The data directory and the paths inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir {
    root: PathBuf,
    memory_override: Option<PathBuf>,
}

impl DataDir {
    /// Describes the layout under `root` without touching the filesystem.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            memory_override: None,
        }
    }

    /// Describes the layout from `[general] data_dir` and `[memory] drive`.
    pub fn from_config(config: &Config) -> Self {
        Self {
            root: config.general.data_dir.clone(),
            memory_override: config.memory.drive.clone(),
        }
    }

    /// Creates and migrates the directory to [`LAYOUT_VERSION`].
    pub fn open(self) -> Result<Self, LayoutError> {
        migrate(&self.root, MIGRATIONS, LAYOUT_VERSION)?;
        if let Some(m) = &self.memory_override {
            create_private_dir(m).map_err(io_err(m))?;
        }
        Ok(self)
    }

    /// The data directory root.
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// `runs/`.
    pub fn runs(&self) -> PathBuf {
        self.root.join("runs")
    }
    /// `sessions/`.
    pub fn sessions(&self) -> PathBuf {
        self.root.join("sessions")
    }
    /// `checkpoints/`.
    pub fn checkpoints(&self) -> PathBuf {
        self.root.join("checkpoints")
    }
    /// `trust/`: the project trust store.
    pub fn trust(&self) -> PathBuf {
        self.root.join("trust")
    }
    /// The memory drive: `[memory] drive`, else `memory/`.
    pub fn memory(&self) -> PathBuf {
        self.memory_override
            .clone()
            .unwrap_or_else(|| self.root.join("memory"))
    }
    /// `remote-cache/`.
    pub fn remote_cache(&self) -> PathBuf {
        self.root.join("remote-cache")
    }
    /// `model-lists/`: cached live model lists for the model catalog
    /// (`harness_providers::catalog::cache`). Created on first write.
    pub fn model_lists(&self) -> PathBuf {
        self.root.join("model-lists")
    }
    /// `auth/`: the Sign in with ChatGPT token store
    /// (`harness_providers::chatgpt::store`). Created, mode `0700`, on the
    /// first sign-in; always a protected path.
    pub fn auth(&self) -> PathBuf {
        self.root.join("auth")
    }
    /// `logs/` (same as `harness_events::logging::log_dir`).
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    /// The layout version file.
    pub fn layout_version_file(&self) -> PathBuf {
        self.root.join(LAYOUT_VERSION_FILE)
    }

    /// Every internal state directory, in layout order.
    pub fn internal_dirs(&self) -> Vec<PathBuf> {
        vec![
            self.runs(),
            self.sessions(),
            self.checkpoints(),
            self.trust(),
            self.memory(),
            self.remote_cache(),
            self.logs(),
        ]
    }

    /// Paths agent tools must never read or write: the whole data
    /// directory, each internal directory (which matters when the memory
    /// drive lives elsewhere), the ChatGPT token store ([`DataDir::auth`])
    /// and the config file, if given.
    ///
    /// Each directory entry covers everything beneath it.
    pub fn protected_paths(&self, config_file: Option<&Path>) -> Vec<PathBuf> {
        let mut v = vec![self.root.clone()];
        v.extend(self.internal_dirs());
        v.push(self.auth());
        v.push(self.layout_version_file());
        v.extend(config_file.map(Path::to_path_buf));
        v.sort();
        v.dedup();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn config_location_precedence() {
        let home = Some(Path::new("/home/me"));
        assert_eq!(
            config_location_with(
                env(&[("HARNESS_CONFIG", "/etc/h.ini"), ("XDG_CONFIG_HOME", "/x")]),
                home
            ),
            ConfigLocation::Explicit("/etc/h.ini".into())
        );
        assert_eq!(
            config_location_with(env(&[("XDG_CONFIG_HOME", "/x")]), home),
            ConfigLocation::Default("/x/harness/harness.ini".into())
        );
        assert_eq!(
            config_location_with(
                env(&[("HARNESS_CONFIG", ""), ("XDG_CONFIG_HOME", "rel")]),
                home
            ),
            ConfigLocation::Default("/home/me/.config/harness/harness.ini".into())
        );
    }

    #[test]
    fn default_data_dir_follows_xdg() {
        let home = Some(Path::new("/home/me"));
        assert_eq!(
            default_data_dir_with(env(&[("XDG_DATA_HOME", "/d")]), home),
            PathBuf::from("/d/harness")
        );
        assert_eq!(
            default_data_dir_with(env(&[]), home),
            PathBuf::from("/home/me/.local/share/harness")
        );
    }

    #[test]
    fn open_fresh_dir_creates_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let d = DataDir::new(tmp.path().join("data")).open().unwrap();
        for dir in d.internal_dirs() {
            assert!(dir.is_dir(), "{}", dir.display());
        }
        assert_eq!(read_layout_version(d.root()).unwrap(), Some(LAYOUT_VERSION));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.trust()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        // Re-opening is a no-op.
        DataDir::new(d.root()).open().unwrap();
    }

    #[test]
    fn upgrades_unversioned_dir_and_keeps_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("logs")).unwrap();
        std::fs::write(root.join("logs/harness.2026-10-01.log"), "old").unwrap();
        let before = migrate(root, MIGRATIONS, LAYOUT_VERSION).unwrap();
        assert_eq!(before, 0);
        assert_eq!(read_layout_version(root).unwrap(), Some(1));
        assert_eq!(
            std::fs::read_to_string(root.join("logs/harness.2026-10-01.log")).unwrap(),
            "old"
        );
        assert!(root.join("sessions").is_dir());
    }

    #[test]
    fn multi_step_upgrade_runs_in_order_and_rejects_newer() {
        fn add_a(root: &Path) -> io::Result<()> {
            std::fs::write(root.join("a"), "1")
        }
        fn rename_a_to_b(root: &Path) -> io::Result<()> {
            std::fs::rename(root.join("a"), root.join("b"))
        }
        let steps = [
            Migration {
                from: 0,
                description: "add a",
                apply: add_a,
            },
            Migration {
                from: 1,
                description: "a -> b",
                apply: rename_a_to_b,
            },
        ];
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        assert_eq!(migrate(root, &steps[..1], 1).unwrap(), 0);
        assert!(root.join("a").exists());
        assert_eq!(migrate(root, &steps, 2).unwrap(), 1);
        assert!(root.join("b").exists() && !root.join("a").exists());
        assert_eq!(read_layout_version(root).unwrap(), Some(2));
        assert!(matches!(
            migrate(root, &steps, 1),
            Err(LayoutError::TooNew { found: 2, .. })
        ));
        assert!(matches!(
            migrate(root, &steps, 3),
            Err(LayoutError::NoMigration(2))
        ));
    }

    #[test]
    fn bad_version_file_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(LAYOUT_VERSION_FILE), "banana").unwrap();
        assert!(matches!(
            DataDir::new(tmp.path()).open(),
            Err(LayoutError::BadVersionFile(_))
        ));
    }

    #[test]
    fn internal_dirs_and_config_are_protected() {
        let d = DataDir {
            root: "/data".into(),
            memory_override: Some("/mem".into()),
        };
        let p = d.protected_paths(Some(Path::new("/cfg/harness.ini")));
        for expected in [
            "/data",
            "/data/runs",
            "/data/sessions",
            "/data/checkpoints",
            "/data/trust",
            "/mem",
            "/data/remote-cache",
            "/data/logs",
            "/data/auth",
            "/data/layout-version",
            "/cfg/harness.ini",
        ] {
            assert!(
                p.contains(&PathBuf::from(expected)),
                "{expected} missing from {p:?}"
            );
        }
    }
}
