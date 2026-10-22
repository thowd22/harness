//! A panic is written to the rotating, redacted log file. Own test binary
//! because panic hooks are process-global.

use std::sync::Arc;

use harness_events::logging::{self, LogConfig};
use harness_tui::terminal::{PANIC_TARGET, install_panic_hook_with};

#[test]
fn panic_is_written_to_redacted_log_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = logging::log_dir(tmp.path());
    let cfg = LogConfig::new(&dir)
        .with_filter("info")
        .with_redactor(Arc::new(|s: &str| {
            s.replace("sk-or-TOPSECRET", "[REDACTED]")
        }));
    let subscriber = logging::build_subscriber(&cfg).unwrap();

    std::panic::set_hook(Box::new(|_| {}));
    install_panic_hook_with(|| {});
    let result = tracing::subscriber::with_default(subscriber, || {
        std::panic::catch_unwind(|| panic!("boom-log with key sk-or-TOPSECRET"))
    });
    let _ = std::panic::take_hook();
    assert!(result.is_err());

    let mut logs = String::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        logs.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    assert!(logs.contains("boom-log with key [REDACTED]"), "{logs}");
    assert!(logs.contains(PANIC_TARGET), "{logs}");
    assert!(logs.contains("ERROR"), "{logs}");
    assert!(!logs.contains("TOPSECRET"), "{logs}");
}
