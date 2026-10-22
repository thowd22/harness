//! TASK-3: secrets never appear in Debug output, logs, serialized config or
//! error messages, and insecure files are refused.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use harness_config::ini::{self, IniError};

const INLINE: &str = "sk-ant-INLINE-0123456789";
const CMD_KEY: &str = "sk-or-FROMCMD-9876543210";
const STDERR_TOKEN: &str = "sk-STDERR-LEAK-555";
// The commands assemble the values at run time, so the command text itself
// (which is not secret) does not contain them.
const LEAK_STDERR: &str = "printf '%s-%s' sk-STDERR-LEAK 555 >&2";

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn write_ini(dir: &Path, text: &str, mode: u32) -> PathBuf {
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

fn sample(dir: &Path) -> PathBuf {
    write_ini(
        dir,
        &format!(
            "[provider.anthropic]\napi_key = {INLINE}\n\
             [provider.openrouter]\napi_key_cmd = {LEAK_STDERR}; printf '%s-%s' sk-or-FROMCMD 9876543210\n\
             [provider.openai]\napi_key_cmd = {LEAK_STDERR}; exit 2\n"
        ),
        0o600,
    )
}

#[test]
fn secrets_absent_from_debug_serialized_config_and_logs() {
    let dir = tempfile::tempdir().unwrap();
    let path = sample(dir.path());
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();

    let (debug, json, resolved_errors) = tracing::subscriber::with_default(subscriber, || {
        let cfg = ini::load_file(&path).unwrap();
        let a = cfg.providers["anthropic"]
            .resolve_api_key()
            .unwrap()
            .unwrap();
        assert_eq!(a.expose(), INLINE);
        let o = cfg.providers["openrouter"]
            .resolve_api_key()
            .unwrap()
            .unwrap();
        assert_eq!(o.expose(), CMD_KEY);
        let err = cfg.providers["openai"].resolve_api_key().unwrap_err();
        tracing::info!(?cfg, ?a, ?o, "debug-format everything into the log");
        (
            format!("{cfg:?} {cfg:#?} {a:?} {o} {err:?}"),
            serde_json::to_string(&cfg).unwrap(),
            err.to_string(),
        )
    });

    let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("credential resolved"),
        "logging was captured: {logs}"
    );
    for (what, text) in [
        ("debug", &debug),
        ("json", &json),
        ("error", &resolved_errors),
        ("logs", &logs),
    ] {
        for secret in [INLINE, CMD_KEY, STDERR_TOKEN] {
            assert!(
                !text.contains(secret),
                "{secret} leaked into {what}: {text}"
            );
        }
    }
    assert!(json.contains("[redacted]"));
}

#[test]
fn parse_errors_never_echo_secret_values() {
    let errs = ini::load_str(
        &format!("[provider.anthropic]\napi_key = {INLINE}\napi_kye = {INLINE}\n"),
        None,
    )
    .unwrap_err();
    assert!(!errs.to_string().contains(INLINE), "{errs}");
}

#[cfg(unix)]
#[test]
fn group_or_world_readable_file_with_secrets_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    for mode in [0o644, 0o640, 0o604] {
        let p = write_ini(
            dir.path(),
            &format!("[provider.anthropic]\napi_key = {INLINE}\n"),
            mode,
        );
        let err = ini::load_file(&p).unwrap_err();
        let msg = err.to_string();
        assert!(
            matches!(err.errors()[0], IniError::InsecurePermissions { .. }),
            "{msg}"
        );
        assert!(msg.contains(&format!("chmod 600 {}", p.display())), "{msg}");
        assert!(!msg.contains(INLINE));
    }
    // A readable file without inline secrets loads (env/cmd names are not secrets).
    let p = write_ini(
        dir.path(),
        "[provider.openai]\napi_key_env = OPENAI_API_KEY\n",
        0o644,
    );
    assert!(ini::load_file(&p).is_ok());
}
