//! Diagnostic logging: `tracing` output to a rotating file under the data
//! directory, filtered by `HARNESS_LOG` and passed through a redactor.
//!
//! Logs never go to the terminal, so they cannot corrupt the TUI. Every
//! formatted log line is handed to a [`LineRedactor`] before it reaches disk,
//! so secrets that slip into log messages are scrubbed the same way events are.
//!
//! ```no_run
//! use std::sync::Arc;
//! use harness_events::logging::{self, LogConfig};
//!
//! let cfg = LogConfig::new(logging::log_dir("/home/me/.local/share/harness".as_ref()))
//!     .with_redactor(Arc::new(|line: &str| line.replace("sk-secret", "[REDACTED]")));
//! logging::init(&cfg).expect("logging");
//! tracing::info!("ready");
//! ```

use std::borrow::Cow;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::Subscriber;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Registry};

/// Environment variable holding the log filter (e.g. `debug`,
/// `harness_agent=trace,info`). Uses `tracing` `EnvFilter` syntax.
pub const LOG_ENV: &str = "HARNESS_LOG";
/// Filter used when neither an explicit filter nor `HARNESS_LOG` is set.
pub const DEFAULT_FILTER: &str = "info";
/// Log file name prefix; files are named `harness.YYYY-MM-DD.log`.
pub const LOG_FILE_PREFIX: &str = "harness";
/// Log file name suffix.
pub const LOG_FILE_SUFFIX: &str = "log";
/// Number of rotated log files kept before the oldest is deleted.
pub const MAX_LOG_FILES: usize = 7;

/// Errors raised while setting up logging.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// The filter string (explicit or from `HARNESS_LOG`) is not valid.
    #[error("invalid log filter {filter:?}: {source}")]
    Filter {
        /// The rejected filter string.
        filter: String,
        /// Parser error.
        #[source]
        source: tracing_subscriber::filter::ParseError,
    },
    /// The log directory or file could not be created.
    #[error("cannot open log directory {}: {source}", dir.display())]
    Io {
        /// Directory that failed.
        dir: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The rolling appender rejected its configuration.
    #[error("cannot create rotating log file in {}: {source}", dir.display())]
    Appender {
        /// Directory that failed.
        dir: PathBuf,
        /// Underlying error.
        #[source]
        source: tracing_appender::rolling::InitError,
    },
    /// A global subscriber was already installed.
    #[error("a global tracing subscriber is already set")]
    AlreadySet,
}

/// Scrubs secrets from a formatted log line before it is written.
///
/// The redaction layer (`harness_events::redact`) implements this so logs and
/// events share one set of rules. Any `Fn(&str) -> String` closure also works.
pub trait LineRedactor: Send + Sync + 'static {
    /// Returns `text` with secrets replaced.
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str>;
}

impl<F> LineRedactor for F
where
    F: Fn(&str) -> String + Send + Sync + 'static,
{
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        Cow::Owned(self(text))
    }
}

/// A [`LineRedactor`] that passes text through unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRedaction;

impl LineRedactor for NoRedaction {
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(text)
    }
}

/// Logging configuration.
#[derive(Clone)]
pub struct LogConfig {
    /// Directory that receives the rotating log files.
    pub dir: PathBuf,
    /// Explicit filter; when `None` the value of `HARNESS_LOG` is used,
    /// falling back to [`DEFAULT_FILTER`].
    pub filter: Option<String>,
    /// Redactor applied to every log line.
    pub redactor: Arc<dyn LineRedactor>,
}

impl std::fmt::Debug for LogConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogConfig")
            .field("dir", &self.dir)
            .field("filter", &self.filter)
            .finish_non_exhaustive()
    }
}

impl LogConfig {
    /// Config logging to `dir` with the environment filter and no redaction.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            filter: None,
            redactor: Arc::new(NoRedaction),
        }
    }

    /// Sets an explicit filter, overriding `HARNESS_LOG`.
    pub fn with_filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = Some(filter.into());
        self
    }

    /// Sets the redactor applied to each log line.
    pub fn with_redactor(mut self, redactor: Arc<dyn LineRedactor>) -> Self {
        self.redactor = redactor;
        self
    }
}

/// The log directory inside a harness data directory: `<data_dir>/logs`.
pub fn log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

/// Picks the effective filter: explicit value, else the `HARNESS_LOG` value,
/// else [`DEFAULT_FILTER`]. Empty strings count as unset.
pub fn resolve_filter(explicit: Option<&str>, env: Option<&str>) -> String {
    [explicit, env]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or(DEFAULT_FILTER)
        .to_owned()
}

/// Builds the logging subscriber without installing it.
///
/// Useful for tests (`tracing::subscriber::with_default`); production code
/// calls [`init`].
pub fn build_subscriber(cfg: &LogConfig) -> Result<impl Subscriber + Send + Sync, LogError> {
    let env = std::env::var(LOG_ENV).ok();
    let filter_str = resolve_filter(cfg.filter.as_deref(), env.as_deref());
    let filter = EnvFilter::try_new(&filter_str).map_err(|source| LogError::Filter {
        filter: filter_str.clone(),
        source,
    })?;

    std::fs::create_dir_all(&cfg.dir).map_err(|source| LogError::Io {
        dir: cfg.dir.clone(),
        source,
    })?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .filename_suffix(LOG_FILE_SUFFIX)
        .max_log_files(MAX_LOG_FILES)
        .build(&cfg.dir)
        .map_err(|source| LogError::Appender {
            dir: cfg.dir.clone(),
            source,
        })?;

    let writer = RedactingMakeWriter::new(appender, Arc::clone(&cfg.redactor));
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(writer);
    Ok(Registry::default().with(filter).with(layer))
}

/// Builds the subscriber and installs it as the global default.
pub fn init(cfg: &LogConfig) -> Result<(), LogError> {
    let subscriber = build_subscriber(cfg)?;
    tracing::subscriber::set_global_default(subscriber).map_err(|_| LogError::AlreadySet)
}

/// A [`MakeWriter`] wrapper that redacts each formatted event before it is
/// written to the inner writer.
pub struct RedactingMakeWriter<M> {
    inner: M,
    redactor: Arc<dyn LineRedactor>,
}

impl<M> RedactingMakeWriter<M> {
    /// Wraps `inner`, redacting with `redactor`.
    pub fn new(inner: M, redactor: Arc<dyn LineRedactor>) -> Self {
        Self { inner, redactor }
    }
}

impl<'a, M> MakeWriter<'a> for RedactingMakeWriter<M>
where
    M: MakeWriter<'a>,
{
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.make_writer(),
            redactor: Arc::clone(&self.redactor),
            buf: Vec::new(),
        }
    }
}

/// Writer produced by [`RedactingMakeWriter`].
///
/// One writer is created per event, so the whole formatted event is buffered
/// and redacted at once; secrets cannot be split across partial writes.
pub struct RedactingWriter<W: Write> {
    inner: W,
    redactor: Arc<dyn LineRedactor>,
    buf: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    fn flush_buf(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&self.buf);
        let redacted = self.redactor.redact(&text);
        self.inner.write_all(redacted.as_bytes())?;
        self.buf.clear();
        self.inner.flush()
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buf()
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        // Logging must never panic; a failed write is dropped silently.
        let _ = self.flush_buf();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_logs(dir: &Path) -> String {
        let mut out = String::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with(LOG_FILE_PREFIX), "unexpected file {name}");
            assert!(name.ends_with(LOG_FILE_SUFFIX), "unexpected file {name}");
            out.push_str(&std::fs::read_to_string(path).unwrap());
        }
        out
    }

    #[test]
    fn resolve_filter_precedence() {
        assert_eq!(resolve_filter(Some("trace"), Some("warn")), "trace");
        assert_eq!(resolve_filter(None, Some("warn")), "warn");
        assert_eq!(resolve_filter(Some("  "), Some("debug")), "debug");
        assert_eq!(resolve_filter(None, None), DEFAULT_FILTER);
        assert_eq!(resolve_filter(None, Some("")), DEFAULT_FILTER);
    }

    #[test]
    fn log_dir_is_under_data_dir() {
        assert_eq!(log_dir(Path::new("/d")), PathBuf::from("/d/logs"));
    }

    #[test]
    fn writes_to_rotating_file_in_dir_with_level_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = log_dir(tmp.path());
        let cfg = LogConfig::new(&dir).with_filter("warn");
        let sub = build_subscriber(&cfg).unwrap();
        tracing::subscriber::with_default(sub, || {
            tracing::info!("hidden-info-line");
            tracing::warn!("visible-warn-line");
        });
        let logs = read_logs(&dir);
        assert!(logs.contains("visible-warn-line"), "{logs}");
        assert!(!logs.contains("hidden-info-line"), "{logs}");
        // Plain text, no ANSI escapes.
        assert!(!logs.contains('\u{1b}'), "{logs}");
    }

    #[test]
    fn log_lines_pass_through_redactor() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = LogConfig::new(tmp.path())
            .with_filter("info")
            .with_redactor(Arc::new(|s: &str| s.replace("sk-ant-SECRET", "[REDACTED]")));
        let sub = build_subscriber(&cfg).unwrap();
        tracing::subscriber::with_default(sub, || {
            tracing::info!(key = "sk-ant-SECRET", "calling with sk-ant-SECRET");
        });
        let logs = read_logs(tmp.path());
        assert!(!logs.contains("sk-ant-SECRET"), "{logs}");
        assert_eq!(logs.matches("[REDACTED]").count(), 2, "{logs}");
    }

    #[test]
    fn invalid_filter_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = LogConfig::new(tmp.path()).with_filter("harness=notalevel");
        assert!(matches!(
            build_subscriber(&cfg),
            Err(LogError::Filter { .. })
        ));
    }

    #[test]
    fn redacting_writer_buffers_until_flush() {
        let sink = std::sync::Mutex::new(Vec::<u8>::new());
        {
            let mut w = RedactingWriter {
                inner: io::Cursor::new(Vec::new()),
                redactor: Arc::new(|s: &str| s.replace("abc", "***")),
                buf: Vec::new(),
            };
            w.write_all(b"xa").unwrap();
            w.write_all(b"bcx").unwrap();
            w.flush().unwrap();
            sink.lock().unwrap().extend(w.inner.get_ref());
        }
        assert_eq!(&*sink.lock().unwrap(), b"x***x");
    }
}
