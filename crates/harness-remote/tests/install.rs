//! Helper install and upgrade, using a local `sh` as the "remote" host with
//! a temporary HOME.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use harness_remote::client::{ClientError, ClientOptions, CommandConnector, Connector};
use harness_remote::install::{
    Arch, HelperBinaries, InstallError, InstalledHelper, connect_with_upgrade, ensure_helper,
    remote_helper_path,
};
use harness_remote::protocol::HELPER_VERSION;
use harness_remote::shell::{LocalShell, RemoteShell, ShellOutput};

struct Remote {
    home: tempfile::TempDir,
    shell: LocalShell,
}

fn remote() -> Remote {
    let home = tempfile::tempdir().unwrap();
    let shell = LocalShell {
        env: vec![("HOME".into(), home.path().display().to_string())],
    };
    Remote { home, shell }
}

fn arch() -> Arch {
    Arch::from_uname(std::env::consts::ARCH).expect("tests run on x86_64 or aarch64")
}

fn installed_path(home: &Path, version: &str) -> PathBuf {
    let p = remote_helper_path(version, arch());
    home.join(p.strip_prefix("~/").unwrap())
}

/// A local "build" that is a script running the real helper binary, so
/// the uploaded copy really works.
fn build(dir: &Path, version: &str, extra: &str) -> PathBuf {
    let f = dir.join(HelperBinaries::file_name(version, arch()));
    std::fs::write(
        &f,
        format!(
            "#!/bin/sh\nexec {} \"$@\" {extra}\n",
            env!("CARGO_BIN_EXE_harness-remote-agent")
        ),
    )
    .unwrap();
    f
}

#[tokio::test]
async fn uploads_on_demand_and_verifies_checksum() {
    let r = remote();
    let bins_dir = tempfile::tempdir().unwrap();
    build(bins_dir.path(), "1.0.0", "");
    let bins = HelperBinaries::new(bins_dir.path());

    let first = ensure_helper(&r.shell, &bins, "1.0.0", true, false)
        .await
        .unwrap();
    assert!(first.uploaded);
    assert_eq!(first.arch, arch());
    let path = installed_path(r.home.path(), "1.0.0");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    assert_eq!(
        harness_tools::tracker::content_hash(&std::fs::read(&path).unwrap()),
        first.sha256
    );

    let second = ensure_helper(&r.shell, &bins, "1.0.0", true, false)
        .await
        .unwrap();
    assert!(!second.uploaded, "already installed and intact");

    std::fs::write(&path, "tampered").unwrap();
    let third = ensure_helper(&r.shell, &bins, "1.0.0", true, false)
        .await
        .unwrap();
    assert!(third.uploaded, "a corrupted helper is replaced");
}

#[tokio::test]
async fn new_version_upgrades_and_removes_the_old_one() {
    let r = remote();
    let bins_dir = tempfile::tempdir().unwrap();
    build(bins_dir.path(), "1.0.0", "");
    build(bins_dir.path(), "1.1.0", "");
    let bins = HelperBinaries::new(bins_dir.path());
    ensure_helper(&r.shell, &bins, "1.0.0", true, false)
        .await
        .unwrap();
    let up = ensure_helper(&r.shell, &bins, "1.1.0", true, false)
        .await
        .unwrap();
    assert!(up.uploaded);
    assert!(installed_path(r.home.path(), "1.1.0").exists());
    assert!(
        !installed_path(r.home.path(), "1.0.0").exists(),
        "old version removed"
    );
}

/// Corrupts the upload in transit.
struct Corrupting(LocalShell);

#[async_trait]
impl RemoteShell for Corrupting {
    async fn run(&self, script: &str, stdin: &[u8]) -> std::io::Result<ShellOutput> {
        let mut data = stdin.to_vec();
        if let Some(b) = data.last_mut() {
            *b ^= 0xff;
        }
        self.0.run(script, &data).await
    }
    fn command_for(&self, script: &str) -> (String, Vec<String>) {
        self.0.command_for(script)
    }
    fn host(&self) -> String {
        "corrupting".into()
    }
}

#[tokio::test]
async fn corrupted_upload_is_rejected_and_not_installed() {
    let r = remote();
    let bins_dir = tempfile::tempdir().unwrap();
    build(bins_dir.path(), "1.0.0", "");
    let bins = HelperBinaries::new(bins_dir.path());
    let shell = Corrupting(r.shell.clone());
    let err = ensure_helper(&shell, &bins, "1.0.0", true, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, InstallError::ChecksumMismatch { .. }),
        "{err}"
    );
    let dir = installed_path(r.home.path(), "1.0.0");
    assert!(!dir.exists());
    let leftovers: Vec<_> = std::fs::read_dir(dir.parent().unwrap()).unwrap().collect();
    assert!(leftovers.is_empty(), "temp upload removed");
}

#[tokio::test]
async fn upload_disabled_points_to_fallback() {
    let r = remote();
    let bins_dir = tempfile::tempdir().unwrap();
    build(bins_dir.path(), "1.0.0", "");
    let bins = HelperBinaries::new(bins_dir.path());
    let err = ensure_helper(&r.shell, &bins, "1.0.0", false, false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("shell fallback"), "{err}");
}

fn connector(home: &Path, sock: &Path) -> impl Fn(&InstalledHelper) -> Arc<dyn Connector> {
    let home = home.to_path_buf();
    let sock = sock.to_path_buf();
    move |inst: &InstalledHelper| {
        let program = home.join(inst.path.strip_prefix("~/").unwrap());
        Arc::new(CommandConnector::new(
            program.display().to_string(),
            vec![
                "remote-agent".into(),
                "--stdio".into(),
                "--grace".into(),
                "1".into(),
                "--socket-dir".into(),
                sock.display().to_string(),
            ],
        )) as Arc<dyn Connector>
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_installs_then_talks_to_the_uploaded_helper() {
    let r = remote();
    let sock = socket_dir();
    let bins_dir = tempfile::tempdir().unwrap();
    build(bins_dir.path(), HELPER_VERSION, "");
    let bins = HelperBinaries::new(bins_dir.path());
    let (client, inst) = connect_with_upgrade(
        &r.shell,
        &bins,
        HELPER_VERSION,
        true,
        connector(r.home.path(), sock.path()),
        ClientOptions::new("box"),
    )
    .await
    .unwrap();
    assert!(inst.uploaded);
    assert_eq!(client.hello().unwrap().version, HELPER_VERSION);
}

/// Counts uploads (scripts with stdin).
struct Counting(LocalShell, Arc<AtomicUsize>);

#[async_trait]
impl RemoteShell for Counting {
    async fn run(&self, script: &str, stdin: &[u8]) -> std::io::Result<ShellOutput> {
        if !stdin.is_empty() {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
        self.0.run(script, stdin).await
    }
    fn command_for(&self, script: &str) -> (String, Vec<String>) {
        self.0.command_for(script)
    }
    fn host(&self) -> String {
        "counting".into()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn handshake_version_mismatch_triggers_a_reupload() {
    let r = remote();
    let sock = socket_dir();
    let bins_dir = tempfile::tempdir().unwrap();
    // A stale build that still announces an old version.
    build(bins_dir.path(), HELPER_VERSION, "--advertise-version 0.0.1");
    let bins = HelperBinaries::new(bins_dir.path());
    let uploads = Arc::new(AtomicUsize::new(0));
    let shell = Counting(r.shell.clone(), uploads.clone());
    let err = connect_with_upgrade(
        &shell,
        &bins,
        HELPER_VERSION,
        true,
        connector(r.home.path(), sock.path()),
        ClientOptions::new("box"),
    )
    .await
    .unwrap_err();
    assert_eq!(
        uploads.load(Ordering::SeqCst),
        2,
        "initial upload plus one forced upgrade"
    );
    assert!(
        matches!(err, InstallError::Client(ClientError::VersionMismatch { ref remote, .. }) if remote == "0.0.1"),
        "{err}"
    );

    // Fix the build: the next connect upgrades and works.
    build(bins_dir.path(), HELPER_VERSION, "");
    let (client, inst) = connect_with_upgrade(
        &shell,
        &bins,
        HELPER_VERSION,
        true,
        connector(r.home.path(), sock.path()),
        ClientOptions::new("box"),
    )
    .await
    .unwrap();
    assert!(inst.uploaded);
    assert_eq!(client.hello().unwrap().version, HELPER_VERSION);
}

/// A short directory for the helper's reattach socket. Unix socket paths are
/// capped (104 bytes on macOS, whose `$TMPDIR` alone is ~50), and
/// `helper-<32 hex>.sock` adds 44, so sockets live under `/tmp`, not `$TMPDIR`.
fn socket_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("hs")
        .tempdir_in("/tmp")
        .expect("create a socket dir under /tmp")
}
