//! The panic hook restores the terminal before logging and before the
//! previous hook prints the panic. Lives in its own test binary because panic
//! hooks are process-global.

use std::io;
use std::sync::{Arc, Mutex};

use harness_tui::terminal::install_panic_hook_with;
use tracing_subscriber::fmt::MakeWriter;

type Steps = Arc<Mutex<Vec<&'static str>>>;

/// Records a "log" step whenever tracing writes an event.
#[derive(Clone)]
struct StepWriter(Steps);

impl io::Write for StepWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if String::from_utf8_lossy(buf).contains("boom-order") {
            self.0.lock().unwrap().push("log");
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for StepWriter {
    type Writer = StepWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn restores_terminal_then_logs_then_prints() {
    let steps: Steps = Arc::default();

    // Stand-in for the default hook that prints the panic to stderr.
    let s = Arc::clone(&steps);
    std::panic::set_hook(Box::new(move |_| s.lock().unwrap().push("print")));

    let s = Arc::clone(&steps);
    install_panic_hook_with(move || s.lock().unwrap().push("restore"));

    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(StepWriter(Arc::clone(&steps)))
        .finish();
    let result = tracing::subscriber::with_default(subscriber, || {
        std::panic::catch_unwind(|| panic!("boom-order"))
    });
    let _ = std::panic::take_hook();

    assert!(result.is_err());
    assert_eq!(*steps.lock().unwrap(), vec!["restore", "log", "print"]);
}
