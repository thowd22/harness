//! Shared helpers for the binary tests: a temp harness.ini, data dir and
//! project, and the real `harness` executable.
#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

pub const SECRET: &str = "conformance-secret-value-123";

/// Copies a directory tree.
pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// The workspace root (for fixtures of other crates).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// A test environment.
pub struct Env {
    _tmp: tempfile::TempDir,
    /// Temp root (harness.ini, data/, scripts).
    pub root: PathBuf,
    /// The project directory (cwd of commands).
    pub project: PathBuf,
}

impl Env {
    pub fn new(extra_ini: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("secret.txt"), format!("token={SECRET}\n")).unwrap();
        let ini = format!(
            "[general]\ndata_dir = {}\n\n[provider.anthropic]\napi_key = {SECRET}\n\n{extra_ini}\n",
            root.join("data").display()
        );
        let ini_path = root.join("harness.ini");
        std::fs::write(&ini_path, ini).unwrap();
        std::fs::set_permissions(&ini_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            _tmp: tmp,
            root,
            project,
        }
    }

    /// An environment whose harness.ini is `ini` after `[general] data_dir`.
    pub fn raw(ini: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let text = format!(
            "[general]\ndata_dir = {}\n\n{ini}\n",
            root.join("data").display()
        );
        let ini_path = root.join("harness.ini");
        std::fs::write(&ini_path, text).unwrap();
        std::fs::set_permissions(&ini_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            _tmp: tmp,
            root,
            project,
        }
    }

    /// The global config directory (`$XDG_CONFIG_HOME/harness`).
    pub fn global(&self) -> PathBuf {
        self.root.join("xdg-config").join("harness")
    }

    /// Writes `text` to `rel` under the global config directory.
    pub fn global_file(&self, rel: &str, text: &str) {
        let p = self.global().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// The run directories, oldest first.
    pub fn runs(&self) -> Vec<PathBuf> {
        let dir = self.root.join("data").join("runs");
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|d| d.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    pub fn script(&self, name: &str, yaml: &str) -> PathBuf {
        let p = self.root.join(name);
        std::fs::write(&p, yaml).unwrap();
        p
    }

    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_harness"));
        c.args(args)
            .current_dir(&self.project)
            .env("HARNESS_CONFIG", self.root.join("harness.ini"))
            .env("XDG_CONFIG_HOME", self.root.join("xdg-config"))
            .env("XDG_DATA_HOME", self.root.join("xdg-data"))
            .env("HOME", &self.root)
            .env_remove("HARNESS_LOG");
        c
    }

    pub fn run(&self, script: &Path, extra: &[&str]) -> Output {
        let mut args = vec!["run", "--fake-script", script.to_str().unwrap()];
        args.extend_from_slice(extra);
        self.cmd(&args).output().unwrap()
    }
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

pub fn events(o: &Output) -> Vec<Value> {
    stdout(o)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not a JSON line ({e}): {l}")))
        .collect()
}
