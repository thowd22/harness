//! Step artifacts: files a step hands to later steps, kept in the run
//! directory instead of the repository.
//!
//! Each step instance has a directory `<run dir>/artifacts/<instance>/`
//! holding one file per declared artifact name. Later steps refer to an
//! artifact by name: `{{ steps.plan.artifacts.plan }}` renders its path.
//! Agent steps cannot read the run directory (it is protected), so when an
//! agent's prompt mentions an artifact, its content is inlined into the
//! brief ([`inline_section`]).
//!
//! How each step type produces its artifacts:
//!
//! | Step | Artifact content |
//! |---|---|
//! | agent | the string field of the same name in its structured output |
//! | shell | the file `$ARTIFACTS_DIR/<name>` the command wrote |
//! | verify | the command output |
//! | bugfix-proof | the proof report (JSON) |
//! | workflow | the nested step artifact of the same name |

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Directory under the run directory.
pub const ARTIFACTS_DIR: &str = "artifacts";
/// Environment variable shell steps write their artifacts under.
pub const ARTIFACTS_ENV: &str = "ARTIFACTS_DIR";
/// Largest artifact inlined into an agent brief.
pub const MAX_INLINE_BYTES: usize = 32 * 1024;

/// One stored artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactInfo {
    /// Step instance that wrote it.
    pub step: String,
    /// Artifact name.
    pub name: String,
    /// Absolute path.
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
}

/// The artifacts of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactStore {
    root: PathBuf,
}

fn check_name(kind: &str, name: &str) -> io::Result<()> {
    let ok = !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '[' | ']' | '.'));
    if ok && !name.contains("..") {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid artifact {kind} `{name}`"),
        ))
    }
}

impl ArtifactStore {
    /// The store of the run in `run_dir`.
    pub fn new(run_dir: &Path) -> Self {
        Self {
            root: run_dir.join(ARTIFACTS_DIR),
        }
    }

    /// `<run dir>/artifacts`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory of step instance `step` (created on demand).
    pub fn dir_for(&self, step: &str) -> io::Result<PathBuf> {
        check_name("step", step)?;
        let d = self.root.join(step);
        fs::create_dir_all(&d)?;
        Ok(d)
    }

    /// Where artifact `name` of `step` lives.
    pub fn path(&self, step: &str, name: &str) -> io::Result<PathBuf> {
        check_name("step", step)?;
        check_name("name", name)?;
        Ok(self.root.join(step).join(name))
    }

    /// Writes an artifact atomically; returns its path.
    pub fn write(&self, step: &str, name: &str, data: &[u8]) -> io::Result<PathBuf> {
        let path = self.path(step, name)?;
        let dir = self.dir_for(step)?;
        let tmp = dir.join(format!(".{name}.tmp"));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Reads an artifact.
    pub fn read(&self, step: &str, name: &str) -> io::Result<Vec<u8>> {
        fs::read(self.path(step, name)?)
    }

    /// The artifacts of `step`, sorted by name.
    pub fn list(&self, step: &str) -> io::Result<Vec<ArtifactInfo>> {
        check_name("step", step)?;
        let dir = self.root.join(step);
        let mut out = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !entry.file_type()?.is_file() {
                continue;
            }
            out.push(ArtifactInfo {
                step: step.to_owned(),
                size: entry.metadata()?.len(),
                path: entry.path(),
                name,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Every artifact of the run, by step instance.
    pub fn list_all(&self) -> io::Result<BTreeMap<String, Vec<ArtifactInfo>>> {
        let mut out = BTreeMap::new();
        let entries = match fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let step = entry.file_name().to_string_lossy().into_owned();
                let list = self.list(&step)?;
                if !list.is_empty() {
                    out.insert(step, list);
                }
            }
        }
        Ok(out)
    }

    /// Copies `step`'s artifacts from another run (resume / re-run) and
    /// returns name to new path.
    pub fn copy_from(
        &self,
        other: &ArtifactStore,
        step: &str,
    ) -> io::Result<BTreeMap<String, PathBuf>> {
        let mut out = BTreeMap::new();
        for a in other.list(step)? {
            let data = fs::read(&a.path)?;
            out.insert(a.name.clone(), self.write(step, &a.name, &data)?);
        }
        Ok(out)
    }
}

/// The artifacts of the step (or step instance) whose node is `node`, in
/// the run in `run_dir`: what the node detail view lists. Uses the
/// workflow state to map nodes to steps.
pub fn for_node(run_dir: &Path, node: &harness_events::schema::NodeId) -> Vec<ArtifactInfo> {
    let store = ArtifactStore::new(run_dir);
    let Ok(state) = crate::rerun::WorkflowState::load(run_dir) else {
        return Vec::new();
    };
    let wf_node = state
        .node
        .as_ref()
        .map(|n| n.as_str().to_owned())
        .unwrap_or_default();
    // Instance nodes are `<workflow node>/<step>[i]`, keyed like their dir.
    if let Some(key) = node.as_str().strip_prefix(&format!("{wf_node}/"))
        && let Ok(list) = store.list(key)
        && !list.is_empty()
    {
        return list;
    }
    state
        .steps
        .iter()
        .find(|(_, r)| r.node.as_ref() == Some(node))
        .map(|(id, r)| {
            r.artifacts
                .iter()
                .map(|(name, path)| ArtifactInfo {
                    step: id.clone(),
                    name: name.clone(),
                    size: fs::metadata(path).map(|m| m.len()).unwrap_or(0),
                    path: path.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Lines for a node-detail view: `name  size  path`.
pub fn view_lines(list: &[ArtifactInfo]) -> Vec<String> {
    list.iter()
        .map(|a| format!("{}  {} B  {}", a.name, a.size, a.path.display()))
        .collect()
}

/// A brief section with the content of `artifacts` (name and path), each
/// cut at [`MAX_INLINE_BYTES`]. Empty when there are none.
pub fn inline_section(artifacts: &[(String, PathBuf)]) -> String {
    let mut s = String::new();
    for (name, path) in artifacts {
        let Ok(data) = fs::read(path) else { continue };
        let cut = data.len() > MAX_INLINE_BYTES;
        let text = String::from_utf8_lossy(&data[..data.len().min(MAX_INLINE_BYTES)]);
        if s.is_empty() {
            s.push_str("\n\nArtifacts from earlier steps:\n");
        }
        s.push_str(&format!(
            "\n--- artifact `{name}` ({}) ---\n",
            path.display()
        ));
        s.push_str(text.trim_end());
        if cut {
            s.push_str("\n[... truncated ...]");
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_list_read_copy() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());
        let p = store.write("plan", "plan", b"1. do it").unwrap();
        assert_eq!(p, dir.path().join("artifacts/plan/plan"));
        store.write("plan", "notes", b"x").unwrap();
        assert_eq!(store.read("plan", "plan").unwrap(), b"1. do it");
        let list = store.list("plan").unwrap();
        assert_eq!(
            list.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["notes", "plan"]
        );
        assert_eq!(list[1].size, 8);
        assert!(view_lines(&list)[1].starts_with("plan  8 B  "));
        assert_eq!(store.list_all().unwrap().len(), 1);

        let other = tempfile::tempdir().unwrap();
        let next = ArtifactStore::new(other.path());
        let copied = next.copy_from(&store, "plan").unwrap();
        assert_eq!(fs::read(&copied["plan"]).unwrap(), b"1. do it");
        assert!(store.write("../x", "a", b"").is_err());
        assert!(store.write("s", "../a", b"").is_err());
        assert!(next.list("missing").unwrap().is_empty());
    }

    #[test]
    fn artifacts_by_node() {
        use crate::rerun::{StepRecord, WorkflowState};
        use harness_events::schema::NodeId;
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());
        let p = store.write("plan", "plan", b"x").unwrap();
        store.write("lint[1]", "log", b"yy").unwrap();
        let mut st = WorkflowState {
            node: Some(NodeId::from("wf")),
            ..WorkflowState::default()
        };
        st.steps.insert(
            "plan".into(),
            StepRecord {
                node: Some(NodeId::from("wf/plan")),
                artifacts: [("plan".to_owned(), p)].into(),
                ..StepRecord::default()
            },
        );
        st.save(dir.path()).unwrap();
        let a = for_node(dir.path(), &NodeId::from("wf/plan"));
        assert_eq!((a.len(), a[0].name.as_str(), a[0].size), (1, "plan", 1));
        let b = for_node(dir.path(), &NodeId::from("wf/lint[1]"));
        assert_eq!(b[0].name, "log");
        assert!(for_node(dir.path(), &NodeId::from("other")).is_empty());
    }

    #[test]
    fn inline_section_includes_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(dir.path());
        let p = store.write("plan", "plan", b"step one\n").unwrap();
        let big = store
            .write("plan", "big", &vec![b'a'; MAX_INLINE_BYTES + 10])
            .unwrap();
        let s = inline_section(&[("plan".into(), p), ("big".into(), big)]);
        assert!(s.contains("--- artifact `plan`"));
        assert!(s.contains("step one"));
        assert!(s.contains("[... truncated ...]"));
        assert_eq!(inline_section(&[]), "");
    }
}
