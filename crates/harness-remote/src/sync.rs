//! File synchronization to and from remote hosts.
//!
//! Lifecycle of a remote node's workspace:
//!
//! 1. **Push.** The local worktree (tracked and untracked files, honouring
//!    `.gitignore`, never `.git/`) is listed by [`manifest`] and copied to a
//!    per-run directory `<workdir>/<project-key>/<run-id>` ([`run_dir`]) with
//!    rsync ([`rsync_args`]) or a git bundle ([`create_bundle`]).
//! 2. **Baseline.** The helper's `sync.manifest` of the remote copy is kept.
//! 3. **Pull.** When the node finishes, the helper's `sync.changes` returns a
//!    [`ChangeSet`] against the baseline.
//! 4. **Apply.** [`apply_changes`] runs every [`ChangeGate`] (policy, hygiene)
//!    and checks each file against the local baseline. Any conflict with a
//!    concurrent local edit, rejection or pending approval aborts the whole
//!    apply, so local files are never overwritten; the change set can still
//!    be saved as a patch ([`ChangeSet::to_patch`]).
//! 5. **Cleanup.** The remote run dir is removed according to
//!    [`CleanupPolicy`]; old run dirs are pruned with [`prune_runs`].

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use harness_policy::rules::{Action, Decision, Mode, PolicyEngine};
use harness_tools::tracker::content_hash;
use serde::{Deserialize, Serialize};

use crate::link::RsyncCompression;
use crate::protocol::b64;
use crate::ssh::{SshTarget, shell_quote};

/// Default remote workdir when `[remote.<name>] workdir` is unset.
pub const DEFAULT_WORKDIR: &str = "~/.cache/harness/work";

/// One file in a [`Manifest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// sha256 of the contents (of the link target text for symlinks).
    pub sha256: String,
    /// Permission bits.
    pub mode: u32,
    /// Size in bytes.
    pub len: u64,
    /// Link target, for symbolic links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink: Option<String>,
}

/// Every syncable file under a root, keyed by `/`-separated relative path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Entries.
    pub entries: BTreeMap<String, ManifestEntry>,
}

impl Manifest {
    /// Relative paths, sorted.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}

/// List the syncable files under `root`: regular files and symlinks,
/// honouring `.gitignore`/`.git/info/exclude` (even outside a git repo),
/// including hidden files and skipping `.git/`.
pub fn manifest(root: &Path) -> io::Result<Manifest> {
    let mut entries = BTreeMap::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .ignore(false)
        .parents(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    for item in walker {
        let item = item.map_err(io::Error::other)?;
        let ft = match item.file_type() {
            Some(ft) => ft,
            None => continue,
        };
        if ft.is_dir() {
            continue;
        }
        let path = item.path();
        let rel = path
            .strip_prefix(root)
            .map_err(io::Error::other)?
            .to_string_lossy()
            .replace('\\', "/");
        if rel.is_empty() {
            continue;
        }
        let meta = std::fs::symlink_metadata(path)?;
        let entry = if ft.is_symlink() {
            let target = std::fs::read_link(path)?.to_string_lossy().into_owned();
            ManifestEntry {
                sha256: content_hash(target.as_bytes()),
                mode: 0o777,
                len: target.len() as u64,
                symlink: Some(target),
            }
        } else if ft.is_file() {
            let data = std::fs::read(path)?;
            ManifestEntry {
                sha256: content_hash(&data),
                mode: meta.permissions().mode() & 0o7777,
                len: data.len() as u64,
                symlink: None,
            }
        } else {
            continue;
        };
        entries.insert(rel, entry);
    }
    Ok(Manifest { entries })
}

/// How one file changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaKind {
    /// New file.
    Added,
    /// Contents or mode changed.
    Modified,
    /// Removed.
    Deleted,
}

/// One changed file coming back from a remote node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDelta {
    /// Relative path.
    pub path: String,
    /// Kind of change.
    pub kind: DeltaKind,
    /// Baseline hash (`None` for added files).
    pub before: Option<String>,
    /// New hash (`None` for deleted files).
    pub after: Option<String>,
    /// New permission bits.
    pub mode: Option<u32>,
    /// New contents (not for deletions or symlinks).
    #[serde(default, with = "opt_b64", skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<u8>>,
    /// New link target, for symlinks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink: Option<String>,
}

mod opt_b64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(d) => super::b64::serialize(d, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        use base64::Engine;
        let s: Option<String> = Option::deserialize(d)?;
        s.map(|s| {
            base64::engine::general_purpose::STANDARD
                .decode(s)
                .map_err(serde::de::Error::custom)
        })
        .transpose()
    }
}

/// Every change of a remote run, sorted by path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    /// Changed files.
    pub changes: Vec<FileDelta>,
}

impl ChangeSet {
    /// True when nothing changed.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// A unified diff of the changes against the local files under
    /// `local_root` (binary files are summarised), for review or for saving
    /// when the change set cannot be applied.
    pub fn to_patch(&self, local_root: &Path) -> String {
        let mut out = String::new();
        for d in &self.changes {
            let before = match d.kind {
                DeltaKind::Added => Some(Vec::new()),
                _ => std::fs::read(local_root.join(&d.path)).ok(),
            };
            let after = d.content.clone().unwrap_or_default();
            let before = before.unwrap_or_default();
            match (std::str::from_utf8(&before), std::str::from_utf8(&after)) {
                (Ok(b), Ok(a)) if d.symlink.is_none() => {
                    let diff = harness_tools::write::unified_diff(&d.path, b, a);
                    if diff.is_empty() {
                        out.push_str(&format!(
                            "mode change {}: {:o}\n",
                            d.path,
                            d.mode.unwrap_or(0)
                        ));
                    } else {
                        if d.kind == DeltaKind::Deleted {
                            out.push_str(&format!("deleted file {}\n", d.path));
                        }
                        out.push_str(&diff);
                    }
                }
                _ => out.push_str(&format!(
                    "Binary or symlink {} changed ({:?})\n",
                    d.path, d.kind
                )),
            }
        }
        out
    }
}

/// Compute the changes under `root` relative to `baseline` (runs inside
/// the helper on the remote copy).
pub fn changes(root: &Path, baseline: &Manifest) -> io::Result<ChangeSet> {
    let now = manifest(root)?;
    let mut changes = Vec::new();
    for (path, e) in &now.entries {
        let kind = match baseline.entries.get(path) {
            None => DeltaKind::Added,
            Some(b) if b != e => DeltaKind::Modified,
            Some(_) => continue,
        };
        let content = match e.symlink {
            Some(_) => None,
            None => Some(std::fs::read(root.join(path))?),
        };
        changes.push(FileDelta {
            path: path.clone(),
            kind,
            before: baseline.entries.get(path).map(|b| b.sha256.clone()),
            after: Some(e.sha256.clone()),
            mode: Some(e.mode),
            content,
            symlink: e.symlink.clone(),
        });
    }
    for (path, b) in &baseline.entries {
        if !now.entries.contains_key(path) {
            changes.push(FileDelta {
                path: path.clone(),
                kind: DeltaKind::Deleted,
                before: Some(b.sha256.clone()),
                after: None,
                mode: None,
                content: None,
                symlink: None,
            });
        }
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ChangeSet { changes })
}

/// What a [`ChangeGate`] says about one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// Fine.
    Allow,
    /// Needs the user's approval first.
    Ask(String),
    /// Must not be applied.
    Deny(String),
}

/// A check every returning change must pass before it is applied locally.
pub trait ChangeGate: Send + Sync {
    /// Short name for reports (`policy`, `hygiene`).
    fn name(&self) -> &str;
    /// Judge one change; `abs` is the local absolute path it would touch.
    fn check(&self, delta: &FileDelta, abs: &Path) -> GateDecision;
}

/// Runs every write through the local policy engine as if a local `write`
/// tool made it.
pub struct PolicyGate {
    engine: PolicyEngine,
    mode: Mode,
}

impl PolicyGate {
    /// Gate using `engine` in `mode`.
    pub fn new(engine: PolicyEngine, mode: Mode) -> Self {
        Self { engine, mode }
    }
}

impl ChangeGate for PolicyGate {
    fn name(&self) -> &str {
        "policy"
    }

    fn check(&self, _delta: &FileDelta, abs: &Path) -> GateDecision {
        let action = Action::Write {
            tool: "remote-sync".into(),
            path: abs.to_path_buf(),
        };
        let root = self.engine.root().to_path_buf();
        let v = self.engine.decide(&action, self.mode, &root);
        match v.decision {
            Decision::Allow => GateDecision::Allow,
            Decision::Ask => GateDecision::Ask(v.rule.reason),
            Decision::Deny => GateDecision::Deny(v.rule.reason),
        }
    }
}

/// Default temp/junk file patterns rejected by [`HygieneGate`].
pub const TEMP_PATTERNS: &[&str] = &[
    "**/*.orig",
    "**/*.rej",
    "**/*~",
    "**/.#*",
    "**/*.swp",
    "**/*.swo",
    "**/*.tmp",
    "**/.DS_Store",
    "**/core",
    "**/core.[0-9]*",
];

/// Rejects temp files and, when a scope is set, files outside it.
#[derive(Debug, Clone)]
pub struct HygieneGate {
    temp: globset::GlobSet,
    scope: Option<globset::GlobSet>,
}

impl Default for HygieneGate {
    fn default() -> Self {
        Self::new(&[]).expect("built-in patterns are valid")
    }
}

impl HygieneGate {
    /// Gate with the default temp patterns and an optional scope (agent
    /// `scope` globs, root-relative). An empty scope allows every path.
    pub fn new(scope: &[String]) -> Result<Self, globset::Error> {
        let mut b = globset::GlobSetBuilder::new();
        for p in TEMP_PATTERNS {
            b.add(globset::Glob::new(p)?);
        }
        let scope = if scope.is_empty() {
            None
        } else {
            let mut s = globset::GlobSetBuilder::new();
            for p in scope {
                s.add(globset::Glob::new(p)?);
            }
            Some(s.build()?)
        };
        Ok(Self {
            temp: b.build()?,
            scope,
        })
    }
}

impl ChangeGate for HygieneGate {
    fn name(&self) -> &str {
        "hygiene"
    }

    fn check(&self, delta: &FileDelta, _abs: &Path) -> GateDecision {
        if delta.kind != DeltaKind::Deleted && self.temp.is_match(&delta.path) {
            return GateDecision::Deny(format!("{} looks like a temp file", delta.path));
        }
        if let Some(scope) = &self.scope
            && !scope.is_match(&delta.path)
        {
            return GateDecision::Deny(format!("{} is outside the agent's scope", delta.path));
        }
        GateDecision::Allow
    }
}

/// A file that changed locally while the remote node ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// Relative path.
    pub path: String,
    /// What happened.
    pub reason: String,
}

/// A change a gate refused or wants approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// Relative path.
    pub path: String,
    /// Gate name.
    pub gate: String,
    /// Reason.
    pub reason: String,
}

/// Options for [`apply_changes`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyOptions {
    /// The user approved the changes, so [`GateDecision::Ask`] passes.
    pub approved: bool,
}

/// Outcome of [`apply_changes`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Paths written or deleted (empty unless everything passed).
    pub applied: Vec<String>,
    /// Concurrent local edits; nothing was applied.
    pub conflicts: Vec<Conflict>,
    /// Gate denials; nothing was applied.
    pub rejected: Vec<Rejection>,
    /// Gate asks without approval; nothing was applied.
    pub needs_approval: Vec<Rejection>,
}

impl ApplyReport {
    /// True when the change set was applied.
    pub fn is_applied(&self) -> bool {
        self.conflicts.is_empty() && self.rejected.is_empty() && self.needs_approval.is_empty()
    }

    /// Human-readable lines describing why nothing was applied.
    pub fn problems(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .conflicts
            .iter()
            .map(|c| format!("conflict: {}: {}", c.path, c.reason))
            .collect();
        v.extend(
            self.rejected
                .iter()
                .map(|r| format!("rejected by {}: {}: {}", r.gate, r.path, r.reason)),
        );
        v.extend(
            self.needs_approval
                .iter()
                .map(|r| format!("needs approval ({}): {}: {}", r.gate, r.path, r.reason)),
        );
        v
    }
}

fn local_hash(abs: &Path) -> io::Result<Option<String>> {
    match std::fs::symlink_metadata(abs) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
        Ok(m) if m.file_type().is_symlink() => Ok(Some(content_hash(
            std::fs::read_link(abs)?.to_string_lossy().as_bytes(),
        ))),
        Ok(m) if m.is_dir() => Ok(Some("<directory>".into())),
        Ok(_) => Ok(Some(content_hash(&std::fs::read(abs)?))),
    }
}

fn safe_rel(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty()
        && p.is_relative()
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        && !p.components().any(|c| c.as_os_str() == ".git")
}

/// Apply `set` to `local_root` if, and only if, every change passes every
/// gate and no file changed locally since `baseline` (the manifest of what
/// was pushed). Otherwise nothing is written and the report says why.
pub fn apply_changes(
    local_root: &Path,
    baseline: &Manifest,
    set: &ChangeSet,
    gates: &[&dyn ChangeGate],
    opts: ApplyOptions,
) -> io::Result<ApplyReport> {
    let mut report = ApplyReport::default();
    for d in &set.changes {
        if !safe_rel(&d.path) {
            report.rejected.push(Rejection {
                path: d.path.clone(),
                gate: "sync".into(),
                reason: "path escapes the workspace".into(),
            });
            continue;
        }
        let abs = local_root.join(&d.path);
        for g in gates {
            match g.check(d, &abs) {
                GateDecision::Allow => {}
                GateDecision::Ask(_) if opts.approved => {}
                GateDecision::Ask(reason) => report.needs_approval.push(Rejection {
                    path: d.path.clone(),
                    gate: g.name().into(),
                    reason,
                }),
                GateDecision::Deny(reason) => report.rejected.push(Rejection {
                    path: d.path.clone(),
                    gate: g.name().into(),
                    reason,
                }),
            }
        }
        let current = local_hash(&abs)?;
        let expected = baseline.entries.get(&d.path).map(|e| e.sha256.clone());
        if current != expected && current != d.after {
            let reason = match (&expected, &current) {
                (None, Some(_)) => "file was created locally during the remote run".to_owned(),
                (Some(_), None) => "file was deleted locally during the remote run".to_owned(),
                _ => "file was edited locally during the remote run".to_owned(),
            };
            report.conflicts.push(Conflict {
                path: d.path.clone(),
                reason,
            });
        }
    }
    if !report.is_applied() {
        return Ok(report);
    }
    for d in &set.changes {
        let abs = local_root.join(&d.path);
        if local_hash(&abs)? == d.after {
            report.applied.push(d.path.clone());
            continue;
        }
        match d.kind {
            DeltaKind::Deleted => match std::fs::remove_file(&abs) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            },
            _ => write_delta(&abs, d)?,
        }
        report.applied.push(d.path.clone());
    }
    Ok(report)
}

fn write_delta(abs: &Path, d: &FileDelta) -> io::Result<()> {
    let parent = abs.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    if let Some(target) = &d.symlink {
        let _ = std::fs::remove_file(abs);
        return std::os::unix::fs::symlink(target, abs);
    }
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::write(tmp.path(), d.content.as_deref().unwrap_or_default())?;
    std::fs::set_permissions(
        tmp.path(),
        std::fs::Permissions::from_mode(d.mode.unwrap_or(0o644) & 0o7777),
    )?;
    tmp.persist(abs).map_err(|e| e.error)?;
    Ok(())
}

/// `<basename>-<12 hex of sha256(path)>`: stable per local project root.
pub fn project_key(root: &Path) -> String {
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let name: String = canon
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".into())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let hash = content_hash(canon.to_string_lossy().as_bytes());
    format!("{name}-{}", &hash[..12])
}

/// `<workdir>/<project-key>` on the remote host.
pub fn project_dir(workdir: Option<&str>, project_key: &str) -> String {
    let w = workdir.unwrap_or(DEFAULT_WORKDIR).trim_end_matches('/');
    format!("{w}/{project_key}")
}

/// `<workdir>/<project-key>/<run-id>` on the remote host.
pub fn run_dir(workdir: Option<&str>, project_key: &str, run_id: &str) -> String {
    format!("{}/{run_id}", project_dir(workdir, project_key))
}

/// Arguments for `rsync` copying the files of `manifest` (fed on stdin with
/// `--files-from=- --from0`, see [`rsync_files_from`]) from `local_root` to
/// `dest` (a remote path when `ssh` is given, otherwise a local path),
/// compressing with `compress` (see [`RsyncCompression::choose`]).
pub fn rsync_args(
    local_root: &Path,
    dest: &str,
    ssh: Option<&SshTarget>,
    compress: RsyncCompression,
) -> Vec<String> {
    let mut args: Vec<String> = vec!["--archive".to_string()];
    args.extend(compress.args());
    args.extend(
        ["--from0", "--files-from=-", "--no-specials", "--no-devices"]
            .iter()
            .map(|s| s.to_string()),
    );
    let src = format!("{}/", local_root.display());
    match ssh {
        Some(t) => {
            args.push("-e".into());
            args.push(t.rsync_shell());
            // rsync passes the remote path through the remote shell.
            let path = match dest.strip_prefix("~/") {
                Some(rest) => format!("~/{}", shell_quote(rest)),
                None => shell_quote(dest),
            };
            // Create the destination first. `--mkpath` would do it, but it
            // needs rsync 3.2.3+, and macOS ships an older rsync.
            args.push(format!("--rsync-path=mkdir -p {path} && rsync"));
            args.push(src);
            args.push(format!("{}:{}/", t.host, path));
        }
        None => {
            args.push(src);
            args.push(format!("{dest}/"));
        }
    }
    args
}

/// The NUL-separated file list for `rsync --files-from=- --from0`.
pub fn rsync_files_from(manifest: &Manifest) -> Vec<u8> {
    let mut out = Vec::new();
    for p in manifest.paths() {
        out.extend_from_slice(p.as_bytes());
        out.push(0);
    }
    out
}

/// The rsync compression for a push to `ssh` (`None`: a local
/// destination): zstd when this rsync and the host's both support it.
pub async fn rsync_compression(ssh: Option<&SshTarget>) -> RsyncCompression {
    let local = crate::link::local_rsync_version().await;
    let remote = match ssh {
        Some(t) => {
            let shell = crate::shell::SshShell { target: t.clone() };
            crate::link::remote_rsync_version(&shell).await
        }
        None => local.clone(),
    };
    RsyncCompression::choose(&local, &remote)
}

/// Run rsync to push `manifest`'s files from `local_root` to `dest`,
/// with zstd compression when both ends support it
/// ([`rsync_compression`]).
pub async fn rsync_push(
    local_root: &Path,
    manifest: &Manifest,
    dest: &str,
    ssh: Option<&SshTarget>,
) -> io::Result<()> {
    use tokio::io::AsyncWriteExt;
    if ssh.is_none() {
        // A remote destination is created through `--rsync-path`.
        tokio::fs::create_dir_all(dest).await?;
    }
    let compress = rsync_compression(ssh).await;
    let mut child = tokio::process::Command::new("rsync")
        .args(rsync_args(local_root, dest, ssh, compress))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let list = rsync_files_from(manifest);
    if let Some(mut si) = child.stdin.take() {
        si.write_all(&list).await?;
    }
    let out = child.wait_with_output().await?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "rsync failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

async fn git(root: &Path, args: &[&str], env: &[(&str, &Path)]) -> io::Result<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(root).args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.stdin(Stdio::null()).output().await?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Write a git bundle of the worktree as it is now (tracked and untracked
/// files, honouring `.gitignore`) to `out`, without touching the user's
/// index, HEAD or branches. Returns the snapshot commit id.
///
/// The snapshot is built in a temporary index and referenced by a
/// temporary `refs/harness/sync/<id>` ref that is deleted again.
pub async fn create_bundle(local_root: &Path, out: &Path) -> io::Result<String> {
    let tmp = tempfile::tempdir()?;
    let index = tmp.path().join("index");
    let env = [("GIT_INDEX_FILE", index.as_path())];
    git(local_root, &["add", "-A", "--", "."], &env).await?;
    let tree = git(local_root, &["write-tree"], &env).await?;
    let commit = {
        let out = tokio::process::Command::new("git")
            .arg("-C")
            .arg(local_root)
            .args(["commit-tree", &tree, "-m", "harness remote sync snapshot"])
            .env("GIT_AUTHOR_NAME", "harness")
            .env("GIT_AUTHOR_EMAIL", "harness@localhost")
            .env("GIT_COMMITTER_NAME", "harness")
            .env("GIT_COMMITTER_EMAIL", "harness@localhost")
            .stdin(Stdio::null())
            .output()
            .await?;
        if !out.status.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ));
        }
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    let refname = format!("refs/harness/sync/{}", uuid::Uuid::now_v7().simple());
    git(local_root, &["update-ref", &refname, &commit], &[]).await?;
    let res = git(
        local_root,
        &["bundle", "create", &out.to_string_lossy(), &refname],
        &[],
    )
    .await;
    let _ = git(local_root, &["update-ref", "-d", &refname], &[]).await;
    res?;
    Ok(commit)
}

/// Shell script that unpacks a bundle at `bundle` into `dest` (run on the
/// remote host after uploading the bundle).
pub fn unbundle_script(bundle: &str, dest: &str) -> String {
    let q = |p: &str| match p.strip_prefix("~/") {
        Some(rest) => format!("\"$HOME\"/{}", shell_quote(rest)),
        None => shell_quote(p),
    };
    format!(
        "set -e; mkdir -p {d}; git -C {d} init -q; git -C {d} fetch -q {b} 'refs/harness/sync/*:refs/harness/sync/*'; \
         git -C {d} checkout -q --detach \"$(git -C {d} for-each-ref --format='%(objectname)' refs/harness/sync | head -n1)\"; rm -f {b}",
        d = q(dest),
        b = q(bundle)
    )
}

/// When a remote run directory is removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CleanupPolicy {
    /// Remove after the node finishes, whatever happened.
    Always,
    /// Remove after success; keep failed runs for inspection (default).
    #[default]
    OnSuccess,
    /// Never remove automatically (pruning still applies).
    Never,
}

impl CleanupPolicy {
    /// Every variant.
    pub const ALL: [CleanupPolicy; 3] = [Self::Always, Self::OnSuccess, Self::Never];

    /// The ini spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::OnSuccess => "on-success",
            Self::Never => "never",
        }
    }

    /// Whether to remove the run dir of a node that `succeeded`.
    pub fn should_remove(self, succeeded: bool) -> bool {
        match self {
            Self::Always => true,
            Self::OnSuccess => succeeded,
            Self::Never => false,
        }
    }
}

impl fmt::Display for CleanupPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CleanupPolicy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| format!("unknown cleanup policy `{s}` (always, on-success, never)"))
    }
}

/// Default number of run dirs kept by pruning.
pub const DEFAULT_KEEP_RUNS: usize = 5;
/// Default maximum age of kept run dirs.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

fn is_strictly_inside(path: &Path, base: &Path) -> bool {
    let p = harness_tools::workspace::normalize(path);
    let b = harness_tools::workspace::normalize(base);
    p != b && p.starts_with(&b) && b.components().count() > 1
}

/// Remove run dir `path`, which must be strictly inside `base` (helper side).
pub fn remove_run_dir(path: &Path, base: &Path) -> io::Result<()> {
    if !is_strictly_inside(path, base) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to remove {}: not inside {}",
                path.display(),
                base.display()
            ),
        ));
    }
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// Remove run dirs under `base` beyond the `keep` newest that are older
/// than `max_age` (or all beyond `keep` when `max_age` is `None`). Returns
/// the removed names (helper side).
pub fn prune_runs(base: &Path, keep: usize, max_age: Option<Duration>) -> io::Result<Vec<String>> {
    let mut dirs: Vec<(SystemTime, PathBuf)> = match std::fs::read_dir(base) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
        Ok(rd) => rd
            .filter_map(Result::ok)
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect(),
    };
    dirs.sort_by(|a, b| b.0.cmp(&a.0));
    let now = SystemTime::now();
    let mut removed = Vec::new();
    for (mtime, path) in dirs.into_iter().skip(keep) {
        let old = match max_age {
            None => true,
            Some(age) => now.duration_since(mtime).unwrap_or_default() > age,
        };
        if old {
            remove_run_dir(&path, base)?;
            removed.push(
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    removed.sort();
    Ok(removed)
}

/// Errors of a [`RemoteRun`].
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// Local I/O or a sync tool (rsync, git) failed.
    #[error("sync: {0}")]
    Io(#[from] io::Error),
    /// The helper call failed.
    #[error("sync: {0}")]
    Client(#[from] crate::client::ClientError),
    /// The remote copy does not match what was pushed.
    #[error("sync incomplete: {} file(s) differ on the remote host (first: {})", .paths.len(), .paths.first().map(String::as_str).unwrap_or("?"))]
    Incomplete {
        /// Paths missing or different on the remote side.
        paths: Vec<String>,
    },
}

/// How the worktree is copied to the remote host.
#[derive(Clone)]
pub enum Pusher {
    /// rsync over ssh (`None`: the "remote" dir is a local path, for tests).
    Rsync(Option<SshTarget>),
    /// git bundle uploaded and unpacked through a shell on the host.
    GitBundle(std::sync::Arc<dyn crate::shell::RemoteShell>),
}

impl fmt::Debug for Pusher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pusher::Rsync(t) => write!(f, "Rsync({:?})", t.as_ref().map(|t| &t.host)),
            Pusher::GitBundle(s) => write!(f, "GitBundle({})", s.host()),
        }
    }
}

/// One remote node's synced workspace.
#[derive(Debug, Clone)]
pub struct RemoteRun {
    client: crate::client::RemoteClient,
    local_root: PathBuf,
    remote_dir: String,
    project_dir: String,
    local_baseline: Manifest,
    remote_baseline: Manifest,
}

/// Copy the files of `local_baseline` to `remote_dir` with `pusher`.
async fn copy_tree(
    local_root: &Path,
    local_baseline: &Manifest,
    project_dir: &str,
    remote_dir: &str,
    run_id: &str,
    pusher: &Pusher,
) -> Result<(), SyncError> {
    match pusher {
        Pusher::Rsync(ssh) => {
            rsync_push(local_root, local_baseline, remote_dir, ssh.as_ref()).await?
        }
        Pusher::GitBundle(shell) => {
            let tmp = tempfile::tempdir()?;
            let bundle = tmp.path().join("sync.bundle");
            create_bundle(local_root, &bundle).await?;
            let data = std::fs::read(&bundle)?;
            let remote_bundle = format!("{project_dir}/.{run_id}.bundle");
            let up = format!(
                "mkdir -p {d} && cat > {b}",
                d = crate::ssh::quote_remote_path(project_dir),
                b = crate::ssh::quote_remote_path(&remote_bundle)
            );
            let out = shell.run(&up, &data).await?;
            if !out.success() {
                return Err(io::Error::other(format!(
                    "bundle upload failed: {}",
                    out.stderr.trim()
                ))
                .into());
            }
            let out = shell
                .run(&unbundle_script(&remote_bundle, remote_dir), b"")
                .await?;
            if !out.success() {
                return Err(
                    io::Error::other(format!("unbundle failed: {}", out.stderr.trim())).into(),
                );
            }
        }
    }
    Ok(())
}

impl RemoteRun {
    /// Push `local_root` to `<workdir>/<project-key>/<run_id>` on the
    /// client's host and record both baselines.
    pub async fn push(
        client: &crate::client::RemoteClient,
        local_root: &Path,
        workdir: Option<&str>,
        run_id: &str,
        pusher: &Pusher,
    ) -> Result<RemoteRun, SyncError> {
        let root = local_root.to_path_buf();
        let local_baseline = tokio::task::spawn_blocking(move || manifest(&root))
            .await
            .map_err(io::Error::other)??;
        let key = project_key(local_root);
        let project_dir = client.expand_home(&project_dir(workdir, &key));
        let remote_dir = format!("{project_dir}/{run_id}");
        let started = std::time::Instant::now();
        let pushed = copy_tree(
            local_root,
            &local_baseline,
            &project_dir,
            &remote_dir,
            run_id,
            pusher,
        )
        .await;
        client.record_audit(
            "sync.push",
            Some(&remote_dir),
            started,
            pushed.as_ref().err().map(|e| e.to_string()),
        );
        pushed?;
        let remote_baseline = client.manifest(&remote_dir).await?;
        let differ: Vec<String> = local_baseline
            .entries
            .iter()
            .filter(|(p, e)| remote_baseline.entries.get(*p).map(|r| &r.sha256) != Some(&e.sha256))
            .map(|(p, _)| p.clone())
            .collect();
        if !differ.is_empty() {
            return Err(SyncError::Incomplete { paths: differ });
        }
        Ok(RemoteRun {
            client: client.clone(),
            local_root: local_root.to_path_buf(),
            remote_dir,
            project_dir,
            local_baseline,
            remote_baseline,
        })
    }

    /// The remote run directory (absolute): the remote workspace root.
    pub fn remote_dir(&self) -> &str {
        &self.remote_dir
    }

    /// `<workdir>/<project-key>` (absolute): the parent of every run dir
    /// of this project on the host.
    pub fn project_dir(&self) -> &str {
        &self.project_dir
    }

    /// What was pushed.
    pub fn local_baseline(&self) -> &Manifest {
        &self.local_baseline
    }

    /// Changes made on the remote host since the push.
    pub async fn pull(&self) -> Result<ChangeSet, SyncError> {
        Ok(self
            .client
            .changes(&self.remote_dir, &self.remote_baseline)
            .await?)
    }

    /// Apply `set` locally through `gates` (see [`apply_changes`]).
    pub fn apply(
        &self,
        set: &ChangeSet,
        gates: &[&dyn ChangeGate],
        opts: ApplyOptions,
    ) -> io::Result<ApplyReport> {
        apply_changes(&self.local_root, &self.local_baseline, set, gates, opts)
    }

    /// Remove the remote run dir if `policy` says so for a node that
    /// `succeeded`. Returns whether it was removed.
    pub async fn cleanup(&self, policy: CleanupPolicy, succeeded: bool) -> Result<bool, SyncError> {
        if !policy.should_remove(succeeded) {
            return Ok(false);
        }
        self.client
            .remove_run_dir(&self.remote_dir, &self.project_dir)
            .await?;
        Ok(true)
    }

    /// Prune old run dirs of this project on the host.
    pub async fn prune(
        &self,
        keep: usize,
        max_age: Option<Duration>,
    ) -> Result<Vec<String>, SyncError> {
        Ok(self.client.prune(&self.project_dir, keep, max_age).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        std::fs::create_dir_all(r.join("src")).unwrap();
        std::fs::create_dir_all(r.join("target/debug")).unwrap();
        std::fs::create_dir_all(r.join(".git")).unwrap();
        std::fs::write(r.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(r.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(r.join("untracked.txt"), "u\n").unwrap();
        std::fs::write(r.join(".env.example"), "X=1\n").unwrap();
        std::fs::write(r.join("target/debug/bin"), "junk").unwrap();
        std::fs::write(r.join("build.log"), "junk").unwrap();
        std::fs::write(r.join(".git/config"), "junk").unwrap();
        t
    }

    #[test]
    fn manifest_honours_gitignore_and_skips_git() {
        let t = tree();
        let m = manifest(t.path()).unwrap();
        let paths: Vec<&str> = m.paths().collect();
        assert_eq!(
            paths,
            vec![".env.example", ".gitignore", "src/main.rs", "untracked.txt"]
        );
        assert_eq!(
            m.entries["src/main.rs"].sha256,
            content_hash(b"fn main() {}\n")
        );
    }

    #[tokio::test]
    async fn rsync_pushes_only_manifest_files() {
        if which_rsync().is_none() {
            eprintln!("rsync not installed; skipping");
            return;
        }
        let t = tree();
        let dest = tempfile::tempdir().unwrap();
        let d = dest.path().join("work/proj/run-1");
        let m = manifest(t.path()).unwrap();
        rsync_push(t.path(), &m, &d.to_string_lossy(), None)
            .await
            .unwrap();
        let got = manifest(&d).unwrap();
        assert_eq!(got, m, "remote copy matches the manifest");
        assert!(!d.join("target").exists());
        assert!(!d.join(".git").exists());
        assert!(!d.join("build.log").exists());
    }

    fn which_rsync() -> Option<()> {
        std::process::Command::new("rsync")
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|_| ())
    }

    #[test]
    fn rsync_args_for_ssh_target() {
        let r = harness_config::ini::Remote {
            host: "gpu.example".into(),
            user: Some("ci".into()),
            port: 22,
            identity_file: None,
            labels: vec![],
            workdir: None,
            max_jobs: 1,
            sync: harness_config::ini::config::SyncMethod::Rsync,
            policy_bash_deny: vec![],
            policy_bash_ask: vec![],
            compression: harness_config::ini::config::SshCompression::Auto,
            compression_below_mbps: 20.0,
        };
        let t = SshTarget::new("gpu", &r, crate::ssh::SshOptions::new("/run/h"));
        let a = rsync_args(
            Path::new("/p"),
            "~/.cache/harness/work/k/r1",
            Some(&t),
            RsyncCompression::Default,
        );
        assert!(a.contains(&"--files-from=-".to_string()));
        assert!(a.contains(&"/p/".to_string()));
        assert_eq!(a.last().unwrap(), "gpu.example:~/.cache/harness/work/k/r1/");
        // Works with rsync older than 3.2.3 (macOS): no --mkpath, the remote
        // shell creates the destination instead.
        assert!(!a.iter().any(|x| x == "--mkpath"), "{a:?}");
        assert!(
            a.contains(&"--rsync-path=mkdir -p ~/.cache/harness/work/k/r1 && rsync".to_string()),
            "{a:?}"
        );
        let e = &a[a.iter().position(|x| x == "-e").unwrap() + 1];
        assert!(e.starts_with("ssh "));
        assert!(e.contains("StrictHostKeyChecking=yes"));
        assert!(e.contains("ForwardAgent=no"));
    }

    #[test]
    fn rsync_args_per_compression_setting() {
        use harness_config::ini::config::SshCompression;
        let base = harness_config::ini::load_str("[remote.gpu]\nhost = gpu.example\n", None)
            .unwrap()
            .remotes
            .remove("gpu")
            .unwrap();
        for (setting, ssh_on) in [
            (SshCompression::Yes, true),
            (SshCompression::No, false),
            (SshCompression::Auto, false),
        ] {
            let r = harness_config::ini::Remote {
                compression: setting,
                ..base.clone()
            };
            let t = SshTarget::new("gpu", &r, crate::ssh::SshOptions::new("/run/h"));
            for (choice, zstd) in [
                (RsyncCompression::Zstd, true),
                (RsyncCompression::Default, false),
            ] {
                let a = rsync_args(Path::new("/p"), "/w", Some(&t), choice);
                assert!(a.contains(&"--compress".to_string()), "{setting}");
                assert_eq!(
                    a.contains(&"--compress-choice=zstd".to_string()),
                    zstd,
                    "{setting} {choice:?}"
                );
                let e = &a[a.iter().position(|x| x == "-e").unwrap() + 1];
                let want = if ssh_on {
                    "Compression=yes"
                } else {
                    "Compression=no"
                };
                assert!(e.contains(want), "{setting}: {e}");
                let socket = if ssh_on {
                    "/run/h/cmz-%C"
                } else {
                    "/run/h/cm-%C"
                };
                assert!(e.contains(socket), "{setting}: {e}");
            }
        }
        // A slow link under `auto` compresses ssh too.
        let auto = SshTarget::new("gpu", &base, crate::ssh::SshOptions::new("/run/h"))
            .with_compression(true);
        let a = rsync_args(Path::new("/p"), "/w", Some(&auto), RsyncCompression::Zstd);
        assert!(a.iter().any(|x| x.contains("Compression=yes")));
    }

    #[tokio::test]
    async fn local_push_compression_follows_the_local_rsync() {
        let local = crate::link::local_rsync_version().await;
        let want = if crate::link::rsync_supports_zstd(&local) {
            RsyncCompression::Zstd
        } else {
            RsyncCompression::Default
        };
        assert_eq!(rsync_compression(None).await, want);
    }

    #[tokio::test]
    async fn git_bundle_carries_untracked_and_respects_ignore() {
        let t = tree();
        std::fs::remove_dir_all(t.path().join(".git")).unwrap();
        let r = t.path();
        let ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(r)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("git not available; skipping");
            return;
        }
        let b = tempfile::tempdir().unwrap();
        let bundle = b.path().join("x.bundle");
        create_bundle(r, &bundle).await.unwrap();
        // The user's repo has no new refs and an untouched index.
        let refs = std::process::Command::new("git")
            .args(["for-each-ref"])
            .current_dir(r)
            .output()
            .unwrap();
        assert!(refs.stdout.is_empty(), "temporary ref removed");
        assert!(!r.join(".git/index").exists(), "user index untouched");
        let dest = b.path().join("dest/run");
        let script = unbundle_script(&bundle.to_string_lossy(), &dest.to_string_lossy());
        let st = std::process::Command::new("sh")
            .args(["-c", &script])
            .status()
            .unwrap();
        assert!(st.success());
        let got = manifest(&dest).unwrap();
        let src = manifest(r).unwrap();
        assert_eq!(got, src);
    }

    fn pushed() -> (tempfile::TempDir, tempfile::TempDir, Manifest) {
        let local = tree();
        let remote = tempfile::tempdir().unwrap();
        copy_tree(local.path(), remote.path());
        let base = manifest(remote.path()).unwrap();
        (local, remote, base)
    }

    fn copy_tree(from: &Path, to: &Path) {
        for p in manifest(from).unwrap().paths() {
            let dst = to.join(p);
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            std::fs::copy(from.join(p), dst).unwrap();
        }
    }

    #[test]
    fn changes_detect_add_modify_delete_and_mode() {
        let (_l, remote, base) = pushed();
        let r = remote.path();
        std::fs::write(r.join("src/main.rs"), "fn main() { println!(); }\n").unwrap();
        std::fs::write(r.join("src/new.rs"), "pub fn x() {}\n").unwrap();
        std::fs::remove_file(r.join("untracked.txt")).unwrap();
        std::fs::set_permissions(
            r.join(".env.example"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let cs = changes(r, &base).unwrap();
        let got: Vec<(&str, DeltaKind)> = cs
            .changes
            .iter()
            .map(|d| (d.path.as_str(), d.kind))
            .collect();
        assert_eq!(
            got,
            vec![
                (".env.example", DeltaKind::Modified),
                ("src/main.rs", DeltaKind::Modified),
                ("src/new.rs", DeltaKind::Added),
                ("untracked.txt", DeltaKind::Deleted),
            ]
        );
        // Survives the wire encoding.
        let json = serde_json::to_string(&cs).unwrap();
        assert_eq!(serde_json::from_str::<ChangeSet>(&json).unwrap(), cs);
    }

    #[test]
    fn clean_changes_apply_locally() {
        let (local, remote, base) = pushed();
        let r = remote.path();
        std::fs::write(r.join("src/main.rs"), "changed\n").unwrap();
        std::fs::write(r.join("src/new.rs"), "new\n").unwrap();
        std::fs::remove_file(r.join("untracked.txt")).unwrap();
        let cs = changes(r, &base).unwrap();
        let patch = cs.to_patch(local.path());
        assert!(patch.contains("+changed"), "{patch}");
        let hyg = HygieneGate::default();
        let rep =
            apply_changes(local.path(), &base, &cs, &[&hyg], ApplyOptions::default()).unwrap();
        assert!(rep.is_applied(), "{:?}", rep.problems());
        assert_eq!(rep.applied.len(), 3);
        let l = local.path();
        assert_eq!(
            std::fs::read_to_string(l.join("src/main.rs")).unwrap(),
            "changed\n"
        );
        assert_eq!(
            std::fs::read_to_string(l.join("src/new.rs")).unwrap(),
            "new\n"
        );
        assert!(!l.join("untracked.txt").exists());
    }

    #[test]
    fn conflicts_are_surfaced_and_nothing_is_overwritten() {
        let (local, remote, base) = pushed();
        std::fs::write(remote.path().join("src/main.rs"), "remote\n").unwrap();
        std::fs::write(remote.path().join("other.txt"), "remote\n").unwrap();
        std::fs::write(local.path().join("src/main.rs"), "local edit\n").unwrap();
        std::fs::write(local.path().join("other.txt"), "local new\n").unwrap();
        let cs = changes(remote.path(), &base).unwrap();
        let rep = apply_changes(local.path(), &base, &cs, &[], ApplyOptions::default()).unwrap();
        assert!(!rep.is_applied());
        assert_eq!(rep.conflicts.len(), 2, "{:?}", rep.conflicts);
        assert!(rep.applied.is_empty());
        assert_eq!(
            std::fs::read_to_string(local.path().join("src/main.rs")).unwrap(),
            "local edit\n",
            "local edit kept"
        );
        assert!(rep.problems()[0].starts_with("conflict:"));
    }

    #[test]
    fn identical_local_change_is_not_a_conflict() {
        let (local, remote, base) = pushed();
        std::fs::write(remote.path().join("src/main.rs"), "same\n").unwrap();
        std::fs::write(local.path().join("src/main.rs"), "same\n").unwrap();
        let cs = changes(remote.path(), &base).unwrap();
        let rep = apply_changes(local.path(), &base, &cs, &[], ApplyOptions::default()).unwrap();
        assert!(rep.is_applied());
    }

    #[test]
    fn hygiene_gate_rejects_temp_files_and_out_of_scope() {
        let (local, remote, base) = pushed();
        std::fs::write(remote.path().join("src/main.rs.orig"), "x").unwrap();
        let cs = changes(remote.path(), &base).unwrap();
        let rep = apply_changes(
            local.path(),
            &base,
            &cs,
            &[&HygieneGate::default()],
            ApplyOptions::default(),
        )
        .unwrap();
        assert_eq!(rep.rejected.len(), 1);
        assert!(!local.path().join("src/main.rs.orig").exists());

        let (local, remote, base) = pushed();
        std::fs::write(remote.path().join("docs.md"), "x").unwrap();
        let cs = changes(remote.path(), &base).unwrap();
        let g = HygieneGate::new(&["src/**".into()]).unwrap();
        let rep = apply_changes(local.path(), &base, &cs, &[&g], ApplyOptions::default()).unwrap();
        assert!(rep.rejected[0].reason.contains("outside the agent's scope"));
    }

    #[test]
    fn policy_gate_blocks_protected_paths_and_asks_in_ask_mode() {
        use harness_policy::rules::PolicyConfig;
        let (local, remote, base) = pushed();
        std::fs::create_dir_all(remote.path().join(".harness")).unwrap();
        std::fs::write(remote.path().join(".harness/agent.yaml"), "x").unwrap();
        std::fs::write(remote.path().join("src/ok.rs"), "x").unwrap();
        let cs = changes(remote.path(), &base).unwrap();
        let cfg = PolicyConfig {
            home: None,
            ..PolicyConfig::default()
        };
        let engine = PolicyEngine::new(&cfg, local.path()).unwrap();
        let gate = PolicyGate::new(engine.clone(), Mode::AutoEdit);
        let rep =
            apply_changes(local.path(), &base, &cs, &[&gate], ApplyOptions::default()).unwrap();
        assert_eq!(rep.rejected.len(), 1, "{:?}", rep);
        assert_eq!(rep.rejected[0].path, ".harness/agent.yaml");
        assert!(!local.path().join("src/ok.rs").exists(), "all or nothing");

        // Ask mode: writes need approval; approval lets them through.
        let (local, remote, base) = pushed();
        std::fs::write(remote.path().join("src/ok.rs"), "x").unwrap();
        let cs = changes(remote.path(), &base).unwrap();
        let engine = PolicyEngine::new(&cfg, local.path()).unwrap();
        let gate = PolicyGate::new(engine, Mode::Ask);
        let rep =
            apply_changes(local.path(), &base, &cs, &[&gate], ApplyOptions::default()).unwrap();
        assert_eq!(rep.needs_approval.len(), 1);
        let rep = apply_changes(
            local.path(),
            &base,
            &cs,
            &[&gate],
            ApplyOptions { approved: true },
        )
        .unwrap();
        assert!(rep.is_applied());
        assert!(local.path().join("src/ok.rs").exists());
    }

    #[test]
    fn escaping_paths_are_rejected() {
        let local = tempfile::tempdir().unwrap();
        let cs = ChangeSet {
            changes: vec![FileDelta {
                path: "../evil".into(),
                kind: DeltaKind::Added,
                before: None,
                after: Some("x".into()),
                mode: Some(0o644),
                content: Some(b"x".to_vec()),
                symlink: None,
            }],
        };
        let rep = apply_changes(
            local.path(),
            &Manifest::default(),
            &cs,
            &[],
            ApplyOptions::default(),
        )
        .unwrap();
        assert_eq!(rep.rejected.len(), 1);
    }

    #[test]
    fn run_dirs_and_project_key() {
        let t = tempfile::tempdir().unwrap();
        let k = project_key(t.path());
        assert_eq!(k, project_key(t.path()), "stable");
        assert_eq!(k.len(), t.path().file_name().unwrap().len() + 13);
        assert_eq!(run_dir(None, "p-1", "r1"), "~/.cache/harness/work/p-1/r1");
        assert_eq!(run_dir(Some("/scratch/"), "p", "r"), "/scratch/p/r");
    }

    #[test]
    fn cleanup_policy() {
        assert!(CleanupPolicy::Always.should_remove(false));
        assert!(CleanupPolicy::OnSuccess.should_remove(true));
        assert!(!CleanupPolicy::OnSuccess.should_remove(false));
        assert!(!CleanupPolicy::Never.should_remove(true));
        assert_eq!(CleanupPolicy::default(), CleanupPolicy::OnSuccess);
        assert_eq!(
            "on-success".parse::<CleanupPolicy>().unwrap(),
            CleanupPolicy::OnSuccess
        );
        assert!("x".parse::<CleanupPolicy>().is_err());
    }

    #[test]
    fn remove_and_prune_stay_inside_base() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().join("proj");
        for i in 0..4 {
            std::fs::create_dir_all(base.join(format!("run{i}"))).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(remove_run_dir(&base, &base).is_err(), "base itself refused");
        assert!(remove_run_dir(t.path(), &base).is_err(), "outside refused");
        assert!(remove_run_dir(&base.join("run0/../../x"), &base).is_err());
        remove_run_dir(&base.join("run0"), &base).unwrap();
        assert!(!base.join("run0").exists());
        let removed = prune_runs(&base, 1, None).unwrap();
        assert_eq!(removed, vec!["run1", "run2"]);
        assert!(base.join("run3").exists());
        assert!(
            prune_runs(&base, 0, Some(Duration::from_secs(3600)))
                .unwrap()
                .is_empty()
        );
    }
}
