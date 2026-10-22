//! Discovery, layering and hot reload of prompts, agents, workflows and skills.
//!
//! Three scopes are searched, later ones shadowing earlier ones by `name`:
//!
//! | Scope | Location |
//! |---|---|
//! | [`Scope::Builtin`] | [`BuiltinFile`]s compiled into the binary |
//! | [`Scope::Global`] | `~/.config/harness/{prompts,agents,workflows,skills}/` ([`global_config_dir`]) |
//! | [`Scope::Project`] | `<repo>/.harness/{...}/`, only when trusted ([`Roots::resolve`]) |
//!
//! The project directory is obtained exclusively through
//! [`TrustStore::trusted_config_dir`]; any trust error means "untrusted".
//! Symlinked directories are never traversed, and inside the project scope
//! symlinked files are skipped too (the trust snapshot only covers the link
//! text, not its target).
//!
//! [`Registry`] holds one kind of artifact; [`Catalog`] holds all four.
//! [`Catalog::reload`] re-scans: a file whose new contents fail to load keeps
//! its previous version and its error is reported with `file:line:col`.
//! [`Watcher`] turns filesystem events into a "reload needed" flag.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::agents::Agent;
use crate::paths::{CONFIG_ENV, config_location_with, home_dir};
use crate::prompts::Prompt;
use crate::skills::Skill;
use crate::trust::TrustStore;
use crate::yaml::{self, YamlError};

/// Where a definition came from. Ordered: later scopes shadow earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Compiled into the binary.
    Builtin,
    /// The user's config directory.
    Global,
    /// The trusted project's `.harness/` directory.
    Project,
}

impl Scope {
    /// All scopes in precedence order (lowest first).
    pub const ALL: [Scope; 3] = [Scope::Builtin, Scope::Global, Scope::Project];

    /// Lower-case label, also used as the slash-command prefix (`/project:name`).
    pub fn label(self) -> &'static str {
        match self {
            Scope::Builtin => "builtin",
            Scope::Global => "global",
            Scope::Project => "project",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// The four kinds of authored artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// `prompts/*.yaml`.
    Prompt,
    /// `agents/*.yaml`.
    Agent,
    /// `workflows/*.yaml`.
    Workflow,
    /// `skills/*.md` or `skills/<name>/SKILL.md`.
    Skill,
}

impl Kind {
    /// The subdirectory name inside a scope root.
    pub fn dir_name(self) -> &'static str {
        match self {
            Kind::Prompt => "prompts",
            Kind::Agent => "agents",
            Kind::Workflow => "workflows",
            Kind::Skill => "skills",
        }
    }

    /// Singular label for messages.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Prompt => "prompt",
            Kind::Agent => "agent",
            Kind::Workflow => "workflow",
            Kind::Skill => "skill",
        }
    }
}

/// A file compiled into the binary, e.g. `include_str!`-ed defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinFile {
    /// Path relative to a scope root, e.g. `prompts/coder.yaml` or `prompts/base.md`.
    pub path: &'static str,
    /// File contents.
    pub contents: &'static str,
}

/// The directories (and built-in files) to search.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    /// Built-in files.
    pub builtins: &'static [BuiltinFile],
    /// Global config directory (contains `prompts/`, `agents/`, ...).
    pub global: Option<PathBuf>,
    /// Trusted project config directory (`<repo>/.harness`).
    pub project: Option<PathBuf>,
    /// The project root trust was checked for (see [`Roots::recheck_trust`]).
    pub project_root: Option<PathBuf>,
    /// Human-readable notes from resolution (e.g. why project config is off).
    pub notes: Vec<String>,
}

impl Roots {
    /// Roots with only built-in files.
    pub fn new(builtins: &'static [BuiltinFile]) -> Self {
        Roots {
            builtins,
            ..Default::default()
        }
    }

    /// Sets the global directory.
    pub fn with_global(mut self, dir: impl Into<PathBuf>) -> Self {
        self.global = Some(dir.into());
        self
    }

    /// Sets the project directory. Callers must have checked trust; prefer
    /// [`Roots::resolve`], which does.
    pub fn with_project(mut self, dir: impl Into<PathBuf>) -> Self {
        self.project = Some(dir.into());
        self
    }

    /// Resolves the standard roots: the global dir from the process
    /// environment and, if `project_root` is given and trusted and unchanged,
    /// its `.harness/` directory.
    pub fn resolve(
        builtins: &'static [BuiltinFile],
        trust: &TrustStore,
        project_root: Option<&Path>,
    ) -> Self {
        let global = global_config_dir(|k| std::env::var_os(k), home_dir().as_deref());
        Self::resolve_with(builtins, Some(global), trust, project_root)
    }

    /// Like [`Roots::resolve`] with an explicit global directory.
    pub fn resolve_with(
        builtins: &'static [BuiltinFile],
        global: Option<PathBuf>,
        trust: &TrustStore,
        project_root: Option<&Path>,
    ) -> Self {
        let mut roots = Roots {
            builtins,
            global,
            project_root: project_root.map(Path::to_path_buf),
            ..Default::default()
        };
        roots.recheck_trust(trust);
        roots
    }

    /// Re-evaluates trust for [`Roots::project_root`]. Call before
    /// [`Catalog::reload`] when files changed: an edit to `.harness/` makes
    /// the project untrusted until the user reviews it again, and the
    /// project scope drops out (with a note) until then.
    pub fn recheck_trust(&mut self, trust: &TrustStore) {
        self.project = None;
        self.notes.clear();
        let Some(root) = self.project_root.clone() else {
            return;
        };
        match trust.trusted_config_dir(&root) {
            Ok(Some(dir)) => self.project = Some(dir),
            Ok(None) => {
                if root.join(crate::trust::PROJECT_CONFIG_DIR).exists() {
                    self.notes.push(format!(
                        "{}: project config is not trusted (or changed since review); using global config only",
                        root.display()
                    ));
                }
            }
            Err(e) => self.notes.push(format!(
                "{}: project config ignored, trust check failed: {e}",
                root.display()
            )),
        }
    }

    fn dir(&self, scope: Scope) -> Option<&Path> {
        match scope {
            Scope::Builtin => None,
            Scope::Global => self.global.as_deref(),
            Scope::Project => self.project.as_deref(),
        }
    }
}

/// The global config directory: the directory of the default `harness.ini`
/// (`$XDG_CONFIG_HOME/harness`, else `~/.config/harness`). `$HARNESS_CONFIG`
/// does not move it.
pub fn global_config_dir(env: impl Fn(&str) -> Option<OsString>, home: Option<&Path>) -> PathBuf {
    let loc = config_location_with(|k| if k == CONFIG_ENV { None } else { env(k) }, home);
    loc.path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// One discovered file, ready to be parsed.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// Scope it was found in.
    pub scope: Scope,
    /// Artifact kind.
    pub kind: Kind,
    /// Disk path (`None` for built-in or in-memory sources).
    pub path: Option<PathBuf>,
    /// Path relative to the scope root, e.g. `agents/impl.yaml`.
    pub rel: String,
    /// File contents.
    pub text: String,
    builtins: &'static [BuiltinFile],
}

impl SourceFile {
    /// An in-memory source (tests, `harness schema` examples). Relative
    /// file references cannot be resolved.
    pub fn inline(
        scope: Scope,
        kind: Kind,
        rel: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        SourceFile {
            scope,
            kind,
            path: None,
            rel: rel.into(),
            text: text.into(),
            builtins: &[],
        }
    }

    /// Reads a file from disk.
    pub fn from_path(scope: Scope, kind: Kind, root: &Path, path: &Path) -> io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        Ok(SourceFile {
            scope,
            kind,
            path: Some(path.to_path_buf()),
            rel,
            text,
            builtins: &[],
        })
    }

    /// A built-in source.
    pub fn builtin(kind: Kind, file: &BuiltinFile, all: &'static [BuiltinFile]) -> Self {
        SourceFile {
            scope: Scope::Builtin,
            kind,
            path: None,
            rel: file.path.to_owned(),
            text: file.contents.to_owned(),
            builtins: all,
        }
    }

    /// The same source with different contents (e.g. an edited buffer).
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    /// Display name: the disk path, or `builtin:<rel>`.
    pub fn display(&self) -> String {
        match &self.path {
            Some(p) => p.display().to_string(),
            None => format!("{}:{}", self.scope.label(), self.rel),
        }
    }

    fn fix_origin(&self, e: YamlError) -> YamlError {
        if self.path.is_none() {
            e.with_origin(self.display())
        } else {
            e
        }
    }

    /// Deserializes the whole file (or `text`) as YAML with located errors.
    pub fn parse_yaml<T: DeserializeOwned>(&self, text: &str) -> Result<T, YamlError> {
        yaml::parse(text, self.path.as_deref()).map_err(|e| self.fix_origin(e))
    }

    /// An error located at `key_path` within this file.
    pub fn error_at(&self, key_path: &[&str], message: impl Into<String>) -> YamlError {
        self.fix_origin(YamlError::at_key(
            self.path.as_deref(),
            &self.text,
            key_path,
            message,
        ))
    }

    /// An error at an explicit line/column.
    pub fn error_at_line(
        &self,
        line: usize,
        column: usize,
        message: impl Into<String>,
    ) -> YamlError {
        self.fix_origin(YamlError::at(self.path.as_deref(), line, column, message))
    }

    /// Reads a file referenced relative to this one (e.g. a prompt layer's
    /// `file: base.md`). Absolute paths and `..` are refused; in the project
    /// scope symlinks are refused.
    pub fn read_relative(&self, rel: &str) -> Result<String, String> {
        let relp = Path::new(rel);
        if rel.is_empty()
            || relp
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(format!(
                "`{rel}` must be a relative path inside this directory (no `/` prefix and no `..`)"
            ));
        }
        match &self.path {
            Some(p) => {
                let dir = p.parent().unwrap_or(Path::new("."));
                let target = dir.join(relp);
                if self.scope == Scope::Project {
                    let mut cur = dir.to_path_buf();
                    for c in relp.components() {
                        cur.push(c);
                        if std::fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink())
                        {
                            return Err(format!(
                                "`{rel}` is a symlink; project config may not use symlinks"
                            ));
                        }
                    }
                }
                std::fs::read_to_string(&target)
                    .map_err(|e| format!("cannot read `{}`: {e}", target.display()))
            }
            None => {
                let dir = Path::new(&self.rel).parent().unwrap_or(Path::new(""));
                let want = dir.join(relp);
                self.builtins
                    .iter()
                    .find(|b| Path::new(b.path) == want)
                    .map(|b| b.contents.to_owned())
                    .ok_or_else(|| format!("`{rel}` not found next to {}", self.display()))
            }
        }
    }
}

/// An authored artifact that can be discovered and layered.
pub trait Artifact: Sized + Clone {
    /// Which directory it lives in.
    const KIND: Kind;
    /// Parses and validates one file (file-local checks only).
    fn load(src: &SourceFile) -> Result<Self, YamlError>;
    /// The name it is registered and shadowed under.
    fn name(&self) -> &str;
}

/// Where a loaded definition came from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Origin {
    /// Scope.
    pub scope: Scope,
    /// Display name (disk path or `builtin:<rel>`).
    pub display: String,
    /// Disk path, if any.
    pub path: Option<PathBuf>,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.display, self.scope)
    }
}

/// A loaded definition and its origin.
#[derive(Debug, Clone)]
pub struct Entry<T> {
    /// The definition.
    pub item: T,
    /// Where it came from.
    pub origin: Origin,
    /// The file text it was loaded from.
    pub text: String,
}

/// A file that failed to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    /// The located error.
    pub error: YamlError,
    /// Artifact kind.
    pub kind: Kind,
    /// True if the previous good version of this file is still in use.
    pub kept_previous: bool,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)?;
        if self.kept_previous {
            write!(f, " (keeping the previous version)")?;
        }
        Ok(())
    }
}

/// A name defined in more than one scope.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Shadowed {
    /// Artifact kind.
    pub kind: Kind,
    /// The shared name.
    pub name: String,
    /// The definition in use (highest scope).
    pub active: Origin,
    /// The hidden definitions, lowest scope first.
    pub hidden: Vec<Origin>,
}

impl fmt::Display for Shadowed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hidden: Vec<String> = self.hidden.iter().map(ToString::to_string).collect();
        write!(
            f,
            "{} `{}` from {} shadows {}",
            self.kind.label(),
            self.name,
            self.active,
            hidden.join(", ")
        )
    }
}

/// Lists the source files of `kind` in every scope, in precedence order and
/// sorted by relative path within a scope. Unreadable entries become notes.
pub fn discover(roots: &Roots, kind: Kind) -> (Vec<SourceFile>, Vec<String>) {
    let mut files = Vec::new();
    let mut notes = Vec::new();
    let mut builtin: Vec<&BuiltinFile> = roots
        .builtins
        .iter()
        .filter(|b| builtin_matches(kind, b.path))
        .collect();
    builtin.sort_by_key(|b| b.path);
    files.extend(
        builtin
            .into_iter()
            .map(|b| SourceFile::builtin(kind, b, roots.builtins)),
    );
    for scope in [Scope::Global, Scope::Project] {
        if let Some(root) = roots.dir(scope) {
            for path in scan_kind_dir(root, kind, scope == Scope::Project, &mut notes) {
                match SourceFile::from_path(scope, kind, root, &path) {
                    Ok(f) => files.push(f),
                    Err(e) => notes.push(format!("{}: cannot read: {e}", path.display())),
                }
            }
        }
    }
    (files, notes)
}

fn is_yaml(name: &str) -> bool {
    name.ends_with(".yaml") || name.ends_with(".yml")
}

fn builtin_matches(kind: Kind, path: &str) -> bool {
    let Some(rest) = path
        .strip_prefix(kind.dir_name())
        .and_then(|r| r.strip_prefix('/'))
    else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    match (kind, parts.as_slice()) {
        (Kind::Skill, [f]) => f.ends_with(".md"),
        (Kind::Skill, [_, "SKILL.md"]) => true,
        (Kind::Skill, _) => false,
        (_, [f]) => is_yaml(f),
        _ => false,
    }
}

fn scan_kind_dir(
    root: &Path,
    kind: Kind,
    no_symlinks: bool,
    notes: &mut Vec<String>,
) -> Vec<PathBuf> {
    let dir = root.join(kind.dir_name());
    // Never traverse a symlinked directory.
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => {}
        Ok(m) if m.file_type().is_symlink() => {
            notes.push(format!("{}: symlinked directory skipped", dir.display()));
            return Vec::new();
        }
        _ => return Vec::new(),
    }
    let Ok(read) = std::fs::read_dir(&dir) else {
        notes.push(format!("{}: cannot list directory", dir.display()));
        return Vec::new();
    };
    let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    let mut out = Vec::new();
    for e in entries {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            if no_symlinks {
                notes.push(format!(
                    "{}: symlink skipped (project config may not use symlinks)",
                    path.display()
                ));
                continue;
            }
            // Global scope: follow symlinked files, never symlinked dirs.
            if !std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
                continue;
            }
            if wanted_file(kind, &name) {
                out.push(path);
            }
        } else if ft.is_file() {
            if wanted_file(kind, &name) {
                out.push(path);
            }
        } else if ft.is_dir() && kind == Kind::Skill {
            let skill = path.join("SKILL.md");
            match std::fs::symlink_metadata(&skill) {
                Ok(m) if m.is_file() => out.push(skill),
                Ok(m) if m.file_type().is_symlink() && !no_symlinks => {
                    if std::fs::metadata(&skill).is_ok_and(|m| m.is_file()) {
                        out.push(skill);
                    }
                }
                Ok(m) if m.file_type().is_symlink() => notes.push(format!(
                    "{}: symlink skipped (project config may not use symlinks)",
                    skill.display()
                )),
                _ => {}
            }
        }
    }
    out
}

fn wanted_file(kind: Kind, name: &str) -> bool {
    match kind {
        Kind::Skill => name.ends_with(".md"),
        _ => is_yaml(name),
    }
}

type FileKey = (Scope, String);

/// All loaded definitions of one kind across scopes.
#[derive(Debug, Clone)]
pub struct Registry<T> {
    files: BTreeMap<FileKey, Entry<T>>,
    errors: Vec<LoadError>,
    notes: Vec<String>,
}

impl<T> Default for Registry<T> {
    fn default() -> Self {
        Registry {
            files: BTreeMap::new(),
            errors: Vec::new(),
            notes: Vec::new(),
        }
    }
}

impl<T: Artifact> Registry<T> {
    /// Discovers and loads every file of `T::KIND`.
    pub fn load(roots: &Roots) -> Self {
        let mut r = Registry::default();
        r.reload(roots);
        r
    }

    /// Re-scans and reloads. Unchanged files are not re-parsed. A file that
    /// fails to load keeps its previous version (if any) and records a
    /// [`LoadError`]. Returns true if any active definition changed.
    pub fn reload(&mut self, roots: &Roots) -> bool {
        let (sources, mut notes) = discover(roots, T::KIND);
        notes.extend(roots.notes.iter().cloned());
        let mut prev = std::mem::take(&mut self.files);
        let mut changed = false;
        self.errors.clear();
        self.notes = notes;
        let mut names: BTreeMap<(Scope, String), String> = BTreeMap::new();
        for src in sources {
            let key = (src.scope, src.rel.clone());
            let old = prev.remove(&key);
            let entry = match old {
                Some(e) if e.text == src.text => Ok(e),
                old => match T::load(&src) {
                    Ok(item) => {
                        changed = true;
                        Ok(Entry {
                            item,
                            origin: Origin {
                                scope: src.scope,
                                display: src.display(),
                                path: src.path.clone(),
                            },
                            text: src.text.clone(),
                        })
                    }
                    Err(error) => {
                        let kept = old.is_some();
                        self.errors.push(LoadError {
                            error,
                            kind: T::KIND,
                            kept_previous: kept,
                        });
                        match old {
                            Some(e) => Ok(e),
                            None => Err(()),
                        }
                    }
                },
            };
            let Ok(entry) = entry else { continue };
            let name_key = (src.scope, entry.item.name().to_owned());
            if let Some(first) = names.get(&name_key) {
                self.errors.push(LoadError {
                    error: src.error_at(
                        &["name"],
                        format!(
                            "duplicate {} name `{}` in the {} scope (already defined in {first})",
                            T::KIND.label(),
                            name_key.1,
                            src.scope
                        ),
                    ),
                    kind: T::KIND,
                    kept_previous: false,
                });
                continue;
            }
            names.insert(name_key, entry.origin.display.clone());
            self.files.insert(key, entry);
        }
        changed || !prev.is_empty()
    }

    /// The active definition of `name` (highest scope wins).
    pub fn get(&self, name: &str) -> Option<&T> {
        self.get_entry(name).map(|e| &e.item)
    }

    /// The active entry for `name`, with its origin.
    pub fn get_entry(&self, name: &str) -> Option<&Entry<T>> {
        self.files
            .values()
            .filter(|e| e.item.name() == name)
            .max_by_key(|e| e.origin.scope)
    }

    /// The definition of `name` in one scope, even if shadowed
    /// (for `/global:name` and `/project:name`).
    pub fn get_in(&self, scope: Scope, name: &str) -> Option<&Entry<T>> {
        self.files
            .values()
            .find(|e| e.origin.scope == scope && e.item.name() == name)
    }

    /// Active entries sorted by name.
    pub fn active(&self) -> Vec<&Entry<T>> {
        let mut by_name: BTreeMap<&str, &Entry<T>> = BTreeMap::new();
        for e in self.files.values() {
            let slot = by_name.entry(e.item.name()).or_insert(e);
            if e.origin.scope > slot.origin.scope {
                *slot = e;
            }
        }
        by_name.into_values().collect()
    }

    /// Every loaded entry, including shadowed ones, in scope order.
    pub fn all(&self) -> Vec<&Entry<T>> {
        let mut v: Vec<&Entry<T>> = self.files.values().collect();
        v.sort_by(|a, b| {
            a.origin
                .scope
                .cmp(&b.origin.scope)
                .then(a.item.name().cmp(b.item.name()))
        });
        v
    }

    /// The entry that `name`'s active definition shadows, if any (the next
    /// lower scope). Used for tighten-only checks.
    pub fn shadowed_by(&self, name: &str) -> Option<&Entry<T>> {
        let active = self.get_entry(name)?;
        self.files
            .values()
            .filter(|e| e.item.name() == name && e.origin.scope < active.origin.scope)
            .max_by_key(|e| e.origin.scope)
    }

    /// Names defined in more than one scope.
    pub fn shadowed(&self) -> Vec<Shadowed> {
        let mut by_name: BTreeMap<&str, Vec<&Entry<T>>> = BTreeMap::new();
        for e in self.files.values() {
            by_name.entry(e.item.name()).or_default().push(e);
        }
        by_name
            .into_iter()
            .filter(|(_, v)| v.len() > 1)
            .map(|(name, mut v)| {
                v.sort_by_key(|e| e.origin.scope);
                let active = v
                    .pop()
                    .map(|e| e.origin.clone())
                    .unwrap_or_else(|| unreachable!());
                Shadowed {
                    kind: T::KIND,
                    name: name.to_owned(),
                    active,
                    hidden: v.into_iter().map(|e| e.origin.clone()).collect(),
                }
            })
            .collect()
    }

    /// Load errors from the last (re)load.
    pub fn errors(&self) -> &[LoadError] {
        &self.errors
    }

    /// Discovery notes (skipped symlinks, unreadable files).
    pub fn notes(&self) -> &[String] {
        &self.notes
    }
}

/// A workflow file, discovered and layered here and parsed fully by
/// `harness-workflow` from [`RawWorkflow::document`].
#[derive(Debug, Clone, PartialEq)]
pub struct RawWorkflow {
    /// `name`.
    pub name: String,
    /// `description`.
    pub description: Option<String>,
    /// Slash command (`command`, defaults to `name`).
    pub command: String,
    /// The whole document as JSON.
    pub document: serde_json::Value,
}

impl Artifact for RawWorkflow {
    const KIND: Kind = Kind::Workflow;

    fn load(src: &SourceFile) -> Result<Self, YamlError> {
        let document: serde_json::Value = src.parse_yaml(&src.text)?;
        let Some(map) = document.as_object() else {
            return Err(src.error_at_line(1, 1, "a workflow file must be a mapping"));
        };
        let field = |k: &str| map.get(k).and_then(|v| v.as_str()).map(str::to_owned);
        let name = field("name")
            .filter(|n| !n.trim().is_empty())
            .ok_or_else(|| {
                src.error_at(
                    &["name"],
                    "missing required field `name` (a non-empty string)",
                )
            })?;
        Ok(RawWorkflow {
            description: field("description"),
            command: field("command").unwrap_or_else(|| name.clone()),
            name,
            document,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Everything discovered: prompts, agents, workflows and skills.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// Prompts.
    pub prompts: Registry<Prompt>,
    /// Agents.
    pub agents: Registry<Agent>,
    /// Workflows (raw).
    pub workflows: Registry<RawWorkflow>,
    /// Skills.
    pub skills: Registry<Skill>,
}

impl Catalog {
    /// Loads all four kinds.
    pub fn load(roots: &Roots) -> Self {
        let mut c = Catalog::default();
        c.reload(roots);
        c
    }

    /// Reloads all four kinds; returns true if anything active changed.
    pub fn reload(&mut self, roots: &Roots) -> bool {
        let a = self.prompts.reload(roots);
        let b = self.agents.reload(roots);
        let c = self.workflows.reload(roots);
        let d = self.skills.reload(roots);
        a | b | c | d
    }

    /// File load errors across all kinds.
    pub fn errors(&self) -> Vec<&LoadError> {
        self.prompts
            .errors()
            .iter()
            .chain(self.agents.errors())
            .chain(self.workflows.errors())
            .chain(self.skills.errors())
            .collect()
    }

    /// Shadowed names across all kinds (for `harness doctor`).
    pub fn shadowed(&self) -> Vec<Shadowed> {
        let mut v = self.prompts.shadowed();
        v.extend(self.agents.shadowed());
        v.extend(self.workflows.shadowed());
        v.extend(self.skills.shadowed());
        v
    }

    /// Discovery notes across all kinds, deduplicated.
    pub fn notes(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .prompts
            .notes()
            .iter()
            .chain(self.agents.notes())
            .chain(self.workflows.notes())
            .chain(self.skills.notes())
            .cloned()
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Cross-file validation of every active agent (model, prompt,
    /// subagent and remote references, and tighten-only rules for agents
    /// that shadow another). See [`crate::agents::validate`].
    pub fn validate_agents(&self, ctx: &crate::agents::ValidationContext) -> Vec<YamlError> {
        self.agents
            .active()
            .into_iter()
            .flat_map(|e| crate::agents::validate(e, self, ctx))
            .collect()
    }
}

/// Watches the global and project roots and reports when a reload is due.
///
/// Symlinks are not followed. Call [`Watcher::take_changed`] (e.g. once per
/// UI tick) and, if true, [`Catalog::reload`].
pub struct Watcher {
    _inner: notify::RecommendedWatcher,
    rx: mpsc::Receiver<()>,
}

impl fmt::Debug for Watcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watcher").finish_non_exhaustive()
    }
}

impl Watcher {
    /// Starts watching every existing root directory recursively.
    pub fn new(roots: &Roots) -> notify::Result<Self> {
        use notify::Watcher as _;
        let (tx, rx) = mpsc::channel();
        let handler = move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res
                && !ev.kind.is_access()
            {
                let _ = tx.send(());
            }
        };
        let config = notify::Config::default().with_follow_symlinks(false);
        let mut inner = notify::RecommendedWatcher::new(handler, config)?;
        for dir in [roots.global.as_deref(), roots.project.as_deref()]
            .into_iter()
            .flatten()
        {
            if dir.is_dir() {
                inner.watch(dir, notify::RecursiveMode::Recursive)?;
            }
        }
        Ok(Watcher { _inner: inner, rx })
    }

    /// True if anything changed since the last call (non-blocking).
    pub fn take_changed(&self) -> bool {
        let mut any = false;
        while self.rx.try_recv().is_ok() {
            any = true;
        }
        any
    }

    /// Blocks up to `timeout` for a change; drains queued events.
    pub fn wait_changed(&self, timeout: Duration) -> bool {
        let got = self.rx.recv_timeout(timeout).is_ok();
        got | self.take_changed()
    }
}

#[cfg(test)]
mod tests;
