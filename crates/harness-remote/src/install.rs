//! Installing and upgrading the remote helper.
//!
//! The helper is the harness binary itself, built statically against musl
//! for `x86_64` and `aarch64` (see `docs/remote.md` for the build). Local
//! builds live in a [`HelperBinaries`] directory as
//! `harness-<version>-<arch>-linux-musl`, optionally with a
//! `<file>.sha256` sidecar.
//!
//! [`ensure_helper`] detects the remote architecture with `uname -m` and
//! compares the sha256 of `~/.cache/harness/bin/<version>-<arch>` with the
//! local build. Only when it is missing or different is the binary uploaded
//! (on demand): it is streamed into a temporary file, checksum-verified on
//! the remote side, and renamed into place, and older versions for that
//! architecture are removed. A version bump therefore always uploads, and
//! [`connect_with_upgrade`] additionally re-uploads once if the running
//! helper still reports a different version in its handshake.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_tools::tracker::content_hash;

use crate::client::{ClientError, ClientOptions, Connector, RemoteClient};
use crate::shell::RemoteShell;

/// Remote directory holding helper binaries.
pub const REMOTE_BIN_DIR: &str = "~/.cache/harness/bin";
/// Environment variable overriding the local helper binary directory.
pub const BIN_DIR_ENV: &str = "HARNESS_REMOTE_BIN_DIR";

/// Helper architectures harness ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arch {
    /// x86_64 / amd64.
    X86_64,
    /// aarch64 / arm64.
    Aarch64,
}

impl Arch {
    /// Both architectures.
    pub const ALL: [Arch; 2] = [Arch::X86_64, Arch::Aarch64];

    /// Parse `uname -m` output.
    pub fn from_uname(s: &str) -> Option<Arch> {
        match s.trim() {
            "x86_64" | "amd64" => Some(Arch::X86_64),
            "aarch64" | "arm64" | "armv8l" => Some(Arch::Aarch64),
            _ => None,
        }
    }

    /// Canonical name.
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }

    /// Rust target triple of the static build.
    pub fn target_triple(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-linux-musl",
            Arch::Aarch64 => "aarch64-unknown-linux-musl",
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `~/.cache/harness/bin/<version>-<arch>`.
pub fn remote_helper_path(version: &str, arch: Arch) -> String {
    format!("{REMOTE_BIN_DIR}/{version}-{arch}")
}

/// Errors while installing the helper.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// Running a remote script failed.
    #[error("remote shell on {host}: {source}")]
    Shell {
        /// Host.
        host: String,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// `uname -m` reported something we have no build for.
    #[error(
        "{host}: unsupported architecture {arch:?}; harness ships x86_64 and aarch64 helpers (use the shell fallback)"
    )]
    UnsupportedArch {
        /// Host.
        host: String,
        /// Reported machine.
        arch: String,
    },
    /// No local build for this version and architecture.
    #[error("no helper build at {}: build it with `cargo build --release --target {}` (see docs/remote.md)", path.display(), arch.target_triple())]
    MissingBinary {
        /// Expected path.
        path: PathBuf,
        /// Architecture.
        arch: Arch,
    },
    /// A checksum did not match.
    #[error("helper checksum mismatch ({place}): expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Where it was checked (`local sidecar`, `after upload`).
        place: String,
        /// Expected sha256.
        expected: String,
        /// Actual sha256.
        actual: String,
    },
    /// The upload script failed.
    #[error("uploading the helper to {host} failed: {message}")]
    UploadFailed {
        /// Host.
        host: String,
        /// stderr.
        message: String,
    },
    /// The helper is missing and uploading is not allowed for this host.
    #[error("the helper is not installed on {host} and upload is disabled; use the shell fallback")]
    UploadDisabled {
        /// Host.
        host: String,
    },
    /// Connecting after install failed.
    #[error(transparent)]
    Client(#[from] ClientError),
}

/// A local helper build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBinary {
    /// File.
    pub path: PathBuf,
    /// sha256 hex.
    pub sha256: String,
}

/// The local directory of static helper builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperBinaries {
    dir: PathBuf,
}

impl HelperBinaries {
    /// Builds in `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `$HARNESS_REMOTE_BIN_DIR`, else `<data_dir>/remote-bin`.
    pub fn from_env_or(data_dir: &Path) -> Self {
        match std::env::var_os(BIN_DIR_ENV).filter(|v| !v.is_empty()) {
            Some(d) => Self::new(d),
            None => Self::new(data_dir.join("remote-bin")),
        }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `harness-<version>-<arch>-linux-musl`.
    pub fn file_name(version: &str, arch: Arch) -> String {
        format!("harness-{version}-{arch}-linux-musl")
    }

    /// Find and checksum the build for `version`/`arch`. A `.sha256`
    /// sidecar, when present, must match.
    pub fn locate(&self, version: &str, arch: Arch) -> Result<LocalBinary, InstallError> {
        let path = self.dir.join(Self::file_name(version, arch));
        let data = std::fs::read(&path).map_err(|_| InstallError::MissingBinary {
            path: path.clone(),
            arch,
        })?;
        let sha256 = content_hash(&data);
        let sidecar = PathBuf::from(format!("{}.sha256", path.display()));
        if let Ok(text) = std::fs::read_to_string(&sidecar) {
            let expected = text.split_whitespace().next().unwrap_or("").to_lowercase();
            if expected != sha256 {
                return Err(InstallError::ChecksumMismatch {
                    place: "local sidecar".into(),
                    expected,
                    actual: sha256,
                });
            }
        }
        Ok(LocalBinary { path, sha256 })
    }
}

/// The result of [`ensure_helper`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledHelper {
    /// Remote path (`~/...`).
    pub path: String,
    /// Remote architecture.
    pub arch: Arch,
    /// sha256 of the installed binary.
    pub sha256: String,
    /// True if this call uploaded it.
    pub uploaded: bool,
}

/// Remote `sh` fragment: `$s` becomes the sha256 of `$f`.
const SHA_OF_F: &str = r#"s=$( (sha256sum -- "$f" 2>/dev/null || shasum -a 256 -- "$f" 2>/dev/null || openssl dgst -sha256 -r "$f") | cut -d' ' -f1)"#;

fn home_path(p: &str) -> String {
    crate::ssh::quote_remote_path(p)
}

fn shell_err(host: String) -> impl FnOnce(io::Error) -> InstallError {
    move |source| InstallError::Shell { host, source }
}

/// The remote architecture (`uname -m`).
pub async fn detect_arch(shell: &dyn RemoteShell) -> Result<Arch, InstallError> {
    let out = shell
        .run("uname -m", b"")
        .await
        .map_err(shell_err(shell.host()))?;
    let m = out.text();
    Arch::from_uname(&m).ok_or(InstallError::UnsupportedArch {
        host: shell.host(),
        arch: m,
    })
}

/// sha256 of a remote file, `None` if it does not exist.
pub async fn remote_sha256(
    shell: &dyn RemoteShell,
    path: &str,
) -> Result<Option<String>, InstallError> {
    let script = format!(
        "f={}; [ -f \"$f\" ] || exit 2; {SHA_OF_F}; echo \"$s\"",
        home_path(path)
    );
    let out = shell
        .run(&script, b"")
        .await
        .map_err(shell_err(shell.host()))?;
    Ok(match out.code {
        Some(0) => Some(out.text()).filter(|s| !s.is_empty()),
        _ => None,
    })
}

/// Make sure the helper for `version` is installed and intact, uploading
/// it when missing or different (always when `force`).
pub async fn ensure_helper(
    shell: &dyn RemoteShell,
    bins: &HelperBinaries,
    version: &str,
    allow_upload: bool,
    force: bool,
) -> Result<InstalledHelper, InstallError> {
    let arch = detect_arch(shell).await?;
    let path = remote_helper_path(version, arch);
    let local = bins.locate(version, arch);
    let current = remote_sha256(shell, &path).await?;
    let local = match local {
        Ok(l) => l,
        // Nothing to compare against: an installed helper is used as is.
        Err(e @ InstallError::MissingBinary { .. }) => {
            return match current {
                Some(sha256) if !force => Ok(InstalledHelper {
                    path,
                    arch,
                    sha256,
                    uploaded: false,
                }),
                _ => Err(e),
            };
        }
        Err(e) => return Err(e),
    };
    if !force && current.as_deref() == Some(local.sha256.as_str()) {
        return Ok(InstalledHelper {
            path,
            arch,
            sha256: local.sha256,
            uploaded: false,
        });
    }
    if !allow_upload {
        return Err(InstallError::UploadDisabled { host: shell.host() });
    }
    let data = std::fs::read(&local.path).map_err(shell_err(shell.host()))?;
    let name = format!("{version}-{arch}");
    let script = format!(
        r#"umask 077
d={dir}; mkdir -p "$d" || exit 1
f="$d/.upload.$$"
cat > "$f" || {{ rm -f "$f"; exit 1; }}
{SHA_OF_F}
if [ "$s" != "{sha}" ]; then rm -f "$f"; echo "$s" >&2; exit 9; fi
chmod 700 "$f" && mv -f "$f" "$d/{name}" || exit 1
for o in "$d"/*-{arch}; do [ "$o" = "$d/{name}" ] || rm -f "$o"; done
exit 0"#,
        dir = home_path(REMOTE_BIN_DIR),
        sha = local.sha256,
    );
    let out = shell
        .run(&script, &data)
        .await
        .map_err(shell_err(shell.host()))?;
    match out.code {
        Some(0) => Ok(InstalledHelper {
            path,
            arch,
            sha256: local.sha256,
            uploaded: true,
        }),
        Some(9) => Err(InstallError::ChecksumMismatch {
            place: "after upload".into(),
            expected: local.sha256,
            actual: out.stderr.trim().to_owned(),
        }),
        _ => Err(InstallError::UploadFailed {
            host: shell.host(),
            message: out.stderr.trim().to_owned(),
        }),
    }
}

/// Install (if needed) and connect. If the handshake reports a version
/// mismatch, the helper is re-uploaded once and the connection retried.
pub async fn connect_with_upgrade<F>(
    shell: &dyn RemoteShell,
    bins: &HelperBinaries,
    version: &str,
    allow_upload: bool,
    make_connector: F,
    opts: ClientOptions,
) -> Result<(RemoteClient, InstalledHelper), InstallError>
where
    F: Fn(&InstalledHelper) -> Arc<dyn Connector>,
{
    let installed = ensure_helper(shell, bins, version, allow_upload, false).await?;
    match RemoteClient::connect(make_connector(&installed), opts.clone()).await {
        Ok(c) => Ok((c, installed)),
        Err(ClientError::VersionMismatch { remote, .. }) => {
            tracing::info!(target: "harness::remote", host = %shell.host(), %remote, "helper version mismatch; upgrading");
            let installed = ensure_helper(shell, bins, version, allow_upload, true).await?;
            let c = RemoteClient::connect(make_connector(&installed), opts).await?;
            Ok((c, installed))
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_parsing() {
        assert_eq!(Arch::from_uname("x86_64\n"), Some(Arch::X86_64));
        assert_eq!(Arch::from_uname("arm64"), Some(Arch::Aarch64));
        assert_eq!(Arch::from_uname("riscv64"), None);
        assert_eq!(Arch::Aarch64.target_triple(), "aarch64-unknown-linux-musl");
        assert_eq!(
            remote_helper_path("1.2.3", Arch::X86_64),
            "~/.cache/harness/bin/1.2.3-x86_64"
        );
    }

    #[test]
    fn locate_checks_sidecar() {
        let t = tempfile::tempdir().unwrap();
        let b = HelperBinaries::new(t.path());
        assert!(matches!(
            b.locate("1.0.0", Arch::X86_64),
            Err(InstallError::MissingBinary { .. })
        ));
        let f = t
            .path()
            .join(HelperBinaries::file_name("1.0.0", Arch::X86_64));
        std::fs::write(&f, b"binary").unwrap();
        let l = b.locate("1.0.0", Arch::X86_64).unwrap();
        assert_eq!(l.sha256, content_hash(b"binary"));
        std::fs::write(
            format!("{}.sha256", f.display()),
            format!("{}  file\n", l.sha256),
        )
        .unwrap();
        assert!(b.locate("1.0.0", Arch::X86_64).is_ok());
        std::fs::write(format!("{}.sha256", f.display()), "deadbeef\n").unwrap();
        assert!(matches!(
            b.locate("1.0.0", Arch::X86_64),
            Err(InstallError::ChecksumMismatch { .. })
        ));
    }
}
