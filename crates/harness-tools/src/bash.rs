//! The `bash` tool: run a shell command non-interactively.
//!
//! - stdin is `/dev/null`; pagers and git credential prompts are disabled.
//! - Every call has a timeout (default `bash_timeout`, capped at
//!   `bash_max_timeout`). Timeout or cancellation (dropping the call's
//!   future) kills the command's whole process group.
//! - Output streams live as [`ToolEvent::Output`] events for the UI; the
//!   model gets the combined output, truncated to head + tail with a
//!   `spill:<id>` pointer when it is over budget (see [`Spiller`]).
//! - The environment is scrubbed of harness secrets and provider keys
//!   ([`EnvScrubber`]).
//! - The working directory persists across calls of one [`BashTool`]
//!   (one per session): `cd sub` in one call affects the next.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::ToolsConfig;
use crate::exec::{Exec, ExecRequest, StreamKind};
use crate::spill::Spiller;
use crate::tool::{ToolContext, ToolError, ToolErrorKind, ToolEvent, ToolOutput, parse_args};

/// Tool name.
pub const NAME: &str = "bash";

/// One-line description shown to the model.
pub const DESCRIPTION: &str = "Run a bash command non-interactively (no stdin). The working directory persists between calls. Optional timeout in seconds.";

/// Arguments of the bash tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BashArgs {
    /// The command line, run with `bash -c` semantics.
    pub command: String,
    /// Timeout in seconds; defaults to `bash_timeout`, capped at `bash_max_timeout`.
    #[serde(default)]
    pub timeout: Option<u64>,
}

/// JSON schema of the bash tool's arguments.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": {"type": "string"},
            "timeout": {"type": "integer", "minimum": 1, "description": "Seconds"}
        },
        "required": ["command"],
        "additionalProperties": false
    })
}

/// Variables set for every command so tools never wait for a human.
pub const NONINTERACTIVE_ENV: &[(&str, &str)] = &[
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_PAGER", "cat"),
    ("PAGER", "cat"),
    ("TERM", "dumb"),
];

/// Provider key variables that are always removed.
pub const DEFAULT_SECRET_NAMES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
];

/// Secret values shorter than this are not used for value matching, to
/// avoid stripping unrelated variables that happen to contain them.
pub const MIN_SECRET_VALUE_LEN: usize = 8;

/// Removes harness secrets and provider keys from a subprocess environment.
///
/// A variable is removed when its name is in [`DEFAULT_SECRET_NAMES`] or the
/// configured names (e.g. every `api_key_env` from `harness.ini`), when its
/// name starts with `HARNESS_`, or when its value contains a known secret
/// value (resolved API keys) of at least [`MIN_SECRET_VALUE_LEN`] bytes.
#[derive(Clone, Default)]
pub struct EnvScrubber {
    names: HashSet<String>,
    values: Vec<String>,
}

impl std::fmt::Debug for EnvScrubber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvScrubber")
            .field("names", &self.names)
            .field("values", &format_args!("[{} redacted]", self.values.len()))
            .finish()
    }
}

impl EnvScrubber {
    /// A scrubber removing the default names plus `names`, and any variable
    /// whose value contains one of `values`.
    pub fn new(
        names: impl IntoIterator<Item = String>,
        values: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut all: HashSet<String> = DEFAULT_SECRET_NAMES.iter().map(|s| s.to_string()).collect();
        all.extend(names);
        Self {
            names: all,
            values: values
                .into_iter()
                .filter(|v| v.len() >= MIN_SECRET_VALUE_LEN)
                .collect(),
        }
    }

    /// True if the variable must not reach a subprocess.
    pub fn is_secret(&self, name: &str, value: &str) -> bool {
        self.names.contains(name)
            || name.starts_with("HARNESS_")
            || self.values.iter().any(|s| value.contains(s.as_str()))
    }

    /// `env` without secret variables.
    pub fn scrub(&self, env: impl IntoIterator<Item = (String, String)>) -> Vec<(String, String)> {
        env.into_iter()
            .filter(|(k, v)| !self.is_secret(k, v))
            .collect()
    }
}

/// Strips the end-of-command marker line (which carries the new working
/// directory) out of a stdout stream, without delaying other output more
/// than the length of a partial marker.
#[derive(Debug)]
struct MarkerFilter {
    needle: Vec<u8>,
    held: Vec<u8>,
    captured: Option<Vec<u8>>,
}

impl MarkerFilter {
    fn new(marker: &str) -> Self {
        Self {
            needle: marker.as_bytes().to_vec(),
            held: Vec::new(),
            captured: None,
        }
    }

    /// Feed a chunk; returns the bytes that are safe to emit now.
    fn feed(&mut self, data: &[u8]) -> Vec<u8> {
        if let Some(c) = &mut self.captured {
            c.extend_from_slice(data);
            return Vec::new();
        }
        self.held.extend_from_slice(data);
        if let Some(pos) = self
            .held
            .windows(self.needle.len())
            .position(|w| w == self.needle.as_slice())
        {
            self.captured = Some(self.held[pos + self.needle.len()..].to_vec());
            self.held.truncate(pos);
            return std::mem::take(&mut self.held);
        }
        // Hold back the longest suffix that could start the marker.
        let max = (self.needle.len() - 1).min(self.held.len());
        let keep = (1..=max)
            .rev()
            .find(|&k| self.held.ends_with(&self.needle[..k]))
            .unwrap_or(0);
        let emit_len = self.held.len() - keep;
        self.held.drain(..emit_len).collect()
    }

    /// End of stream: remaining output and the captured directory, if any.
    fn finish(self) -> (Vec<u8>, Option<String>) {
        let cwd = self.captured.and_then(|c| {
            let s = String::from_utf8(c).ok()?;
            let line = s.strip_suffix('\n').unwrap_or(&s);
            (!line.is_empty()).then(|| line.to_string())
        });
        (self.held, cwd)
    }
}

/// Most output bytes kept in memory per stream end (head and tail each).
const CAPTURE_HALF: usize = 16 * 1024 * 1024;

/// Bounded capture of combined output: keeps the first and last
/// [`CAPTURE_HALF`] bytes and counts what was dropped in between.
#[derive(Debug, Default)]
struct Capture {
    head: Vec<u8>,
    tail: Vec<u8>,
    dropped: usize,
}

impl Capture {
    fn push(&mut self, mut data: &[u8]) {
        if self.head.len() < CAPTURE_HALF {
            let n = data.len().min(CAPTURE_HALF - self.head.len());
            self.head.extend_from_slice(&data[..n]);
            data = &data[n..];
        }
        self.tail.extend_from_slice(data);
        if self.tail.len() > 2 * CAPTURE_HALF {
            let excess = self.tail.len() - CAPTURE_HALF;
            self.tail.drain(..excess);
            self.dropped += excess;
        }
    }

    fn into_string(self) -> String {
        let mut out = String::from_utf8_lossy(&self.head).into_owned();
        if self.dropped > 0 {
            out.push_str(&format!(
                "\n[... {} bytes of output discarded from memory ...]\n",
                self.dropped
            ));
        }
        out.push_str(&String::from_utf8_lossy(&self.tail));
        out
    }
}

/// The bash tool for one session.
pub struct BashTool {
    exec: Arc<dyn Exec>,
    config: ToolsConfig,
    scrubber: EnvScrubber,
    spiller: Spiller,
    base_env: Vec<(String, String)>,
    root: PathBuf,
    cwd: Mutex<PathBuf>,
    marker: String,
}

impl std::fmt::Debug for BashTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BashTool")
            .field("cwd", &self.cwd())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

const SCRIPT: &str = r#"trap '__harness_rc=$?; printf "%s%s\n" "$HARNESS_MARKER_PLACEHOLDER" "$PWD"; exit $__harness_rc' EXIT
cd -- "$1" 2>/dev/null || printf '[harness: working directory %s no longer exists; running in %s]\n' "$1" "$PWD" >&2
eval "$2""#;

impl BashTool {
    /// A bash tool running through `exec`, starting in `cwd`.
    ///
    /// Each process is started in `cwd` as given here (which must exist on
    /// the exec host) and then changes to the session directory.
    ///
    /// The subprocess environment is this process's environment, scrubbed
    /// by `scrubber`, plus [`NONINTERACTIVE_ENV`]. Large outputs go through
    /// `spiller`.
    pub fn new(
        exec: Arc<dyn Exec>,
        cwd: impl Into<PathBuf>,
        config: ToolsConfig,
        scrubber: EnvScrubber,
        spiller: Spiller,
    ) -> Self {
        let cwd = cwd.into();
        Self {
            exec,
            config,
            scrubber,
            spiller,
            base_env: std::env::vars().collect(),
            root: cwd.clone(),
            cwd: Mutex::new(cwd),
            marker: format!("__HARNESS_CWD_{}__:", uuid::Uuid::now_v7().simple()),
        }
    }

    /// Use `env` instead of this process's environment as the base (it is
    /// still scrubbed).
    pub fn with_base_env(mut self, env: Vec<(String, String)>) -> Self {
        self.base_env = env;
        self
    }

    /// The current session working directory.
    pub fn cwd(&self) -> PathBuf {
        self.cwd.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn env(&self) -> Vec<(String, String)> {
        let mut env = self.scrubber.scrub(self.base_env.iter().cloned());
        env.retain(|(k, _)| !NONINTERACTIVE_ENV.iter().any(|(n, _)| n == k));
        env.extend(
            NONINTERACTIVE_ENV
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        env
    }

    /// Run with raw JSON arguments.
    pub async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        self.run(ctx, parse_args(NAME, args)?).await
    }

    /// Run with typed arguments. Dropping the returned future kills the
    /// command's process group.
    pub async fn run(&self, ctx: &ToolContext, args: BashArgs) -> Result<ToolOutput, ToolError> {
        let max = self.config.bash_max_timeout();
        let asked = args.timeout.map(|s| Duration::from_secs(s.max(1)));
        let timeout = asked.unwrap_or(self.config.bash_timeout()).min(max);
        let capped = asked.is_some_and(|a| a > max);

        let cwd = self.cwd();
        let script = SCRIPT.replace("$HARNESS_MARKER_PLACEHOLDER", &self.marker);
        let req = ExecRequest::new(
            "bash",
            vec![
                "--noprofile".into(),
                "--norc".into(),
                "-c".into(),
                script,
                "harness-bash".into(),
                cwd.to_string_lossy().into_owned(),
                args.command,
            ],
            self.root.clone(),
        )
        .with_clean_env(self.env())
        .with_timeout(timeout);

        let mut handle = self.exec.spawn(req).await.map_err(|e| {
            ToolError::new(
                ToolErrorKind::Exec,
                format!("{e}. Check that bash is installed and the working directory exists."),
            )
        })?;
        let mut filter = MarkerFilter::new(&self.marker);
        let mut capture = Capture::default();
        let emit = |stream: StreamKind, data: Vec<u8>, capture: &mut Capture| {
            if data.is_empty() {
                return;
            }
            capture.push(&data);
            ctx.emit(ToolEvent::Output { stream, data });
        };
        while let Some(chunk) = handle.next_output().await {
            let data = match chunk.stream {
                StreamKind::Stdout => filter.feed(&chunk.data),
                StreamKind::Stderr => chunk.data,
            };
            emit(chunk.stream, data, &mut capture);
        }
        let outcome = handle
            .wait()
            .await
            .map_err(|e| ToolError::new(ToolErrorKind::Exec, format!("{e}. Retry the command.")))?;
        let (rest, new_cwd) = filter.finish();
        emit(StreamKind::Stdout, rest, &mut capture);
        if let Some(dir) = new_cwd {
            *self.cwd.lock().unwrap_or_else(|e| e.into_inner()) = PathBuf::from(dir);
        }

        let mut text = self.spiller.apply(ctx, NAME, capture.into_string());
        let mut status = Vec::new();
        if outcome.timed_out {
            status.push(format!(
                "[timed out after {}s: the command and every process it started were killed. Pass a larger timeout (max {}s) if it needs longer, or run something that terminates on its own.]",
                timeout.as_secs(),
                max.as_secs()
            ));
        } else if outcome.killed {
            status
                .push("[cancelled: the command and every process it started were killed.]".into());
        } else if let Some(sig) = outcome.signal {
            status.push(format!("[killed by signal {sig}]"));
        } else if let Some(code) = outcome.code.filter(|c| *c != 0) {
            status.push(format!("[exit code {code}]"));
        }
        if capped {
            status.push(format!(
                "[timeout capped at the {}s maximum]",
                max.as_secs()
            ));
        }
        if text.is_empty() && status.is_empty() {
            text.push_str("[no output]");
        }
        for line in status {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&line);
        }
        Ok(ToolOutput::Text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalExec;
    use crate::spill::{BlobSink, MemoryBlobSink};
    use crate::tool::RecordingEvents;

    struct Fx {
        dir: tempfile::TempDir,
        tool: Arc<BashTool>,
        sink: Arc<MemoryBlobSink>,
        events: Arc<RecordingEvents>,
        ctx: ToolContext,
    }

    fn fx_with(config: ToolsConfig, scrubber: EnvScrubber) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let sink = Arc::new(MemoryBlobSink::new());
        let events = Arc::new(RecordingEvents::new());
        let tool = BashTool::new(
            Arc::new(LocalExec::new()),
            dir.path(),
            config.clone(),
            scrubber,
            Spiller::new(sink.clone(), &config),
        );
        Fx {
            dir,
            tool: Arc::new(tool),
            sink,
            ctx: ToolContext::new("b1").with_events(events.clone()),
            events,
        }
    }

    fn fx() -> Fx {
        fx_with(ToolsConfig::default(), EnvScrubber::default())
    }

    async fn run(f: &Fx, command: &str) -> String {
        f.tool
            .call(&f.ctx, json!({"command": command}))
            .await
            .unwrap()
            .text()
            .to_string()
    }

    #[tokio::test]
    async fn runs_with_stdin_closed_and_reports_exit_code() {
        let f = fx();
        assert_eq!(run(&f, "echo hi").await, "hi\n");
        assert_eq!(run(&f, "read x; echo rc=$?").await, "rc=1\n");
        assert_eq!(
            run(&f, "echo out; echo err >&2; exit 4").await,
            "out\nerr\n[exit code 4]"
        );
        assert_eq!(run(&f, "true").await, "[no output]");
        assert_eq!(run(&f, "printf 'no newline'").await, "no newline");
    }

    #[tokio::test]
    async fn timeout_kills_process_group_and_is_capped() {
        let f = fx_with(
            ToolsConfig {
                bash_timeout: 1,
                bash_max_timeout: 2,
                ..Default::default()
            },
            EnvScrubber::default(),
        );
        let start = std::time::Instant::now();
        let out = run(&f, "sleep 60 & echo $! > pid; wait").await;
        assert!(start.elapsed() < Duration::from_secs(10));
        assert!(out.contains("[timed out after 1s"), "{out}");
        assert!(out.contains("max 2s"), "{out}");
        let pid = std::fs::read_to_string(f.dir.path().join("pid")).unwrap();
        assert_dead(pid.trim()).await;
        let out = f
            .tool
            .call(&f.ctx, json!({"command": "true", "timeout": 999}))
            .await
            .unwrap();
        assert_eq!(out.text(), "[timeout capped at the 2s maximum]");
    }

    async fn assert_dead(pid: &str) {
        let pid: u32 = pid.parse().expect("pid file holds a pid");
        for _ in 0..50 {
            if crate::conformance::local_process_gone(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("process {pid} survived");
    }

    #[tokio::test]
    async fn cancel_kills_process_group() {
        let f = fx();
        let tool = f.tool.clone();
        let ctx = f.ctx.clone();
        let task = tokio::spawn(async move {
            tool.call(&ctx, json!({"command": "sleep 60 & echo $! > pid; wait"}))
                .await
        });
        let pid_file = f.dir.path().join("pid");
        for _ in 0..100 {
            if std::fs::read_to_string(&pid_file).is_ok_and(|s| s.ends_with('\n')) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        task.abort();
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert_dead(pid.trim()).await;
    }

    #[tokio::test]
    async fn streams_live_and_hides_marker() {
        let f = fx();
        run(&f, "echo one; echo two >&2").await;
        let mut out = Vec::new();
        for (id, ev) in f.events.events() {
            assert_eq!(id, "b1");
            if let ToolEvent::Output { data, .. } = ev {
                out.extend(data);
            }
        }
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("one\n") && s.contains("two\n"), "{s}");
        assert!(!s.contains("__HARNESS_CWD"), "{s}");
    }

    #[tokio::test]
    async fn large_output_truncated_with_spill_pointer() {
        let f = fx_with(
            ToolsConfig {
                spill_over_bytes: 2000,
                spill_head_lines: 5,
                spill_tail_lines: 5,
                ..Default::default()
            },
            EnvScrubber::default(),
        );
        let out = run(&f, "seq 1 5000; exit 2").await;
        assert!(
            out.starts_with("1\n2\n3\n4\n5\n[... output truncated"),
            "{out}"
        );
        assert!(out.ends_with("4999\n5000\n[exit code 2]"), "{out}");
        let id = out.split("spill:").nth(1).unwrap()[..16].to_string();
        let full = String::from_utf8(f.sink.get(&id).unwrap().unwrap()).unwrap();
        assert_eq!(full.lines().count(), 5000);
        assert!(
            f.events
                .events()
                .iter()
                .any(|(_, e)| matches!(e, ToolEvent::OutputSpilled(r) if r.tool == "bash"))
        );
    }

    #[tokio::test]
    async fn environment_is_scrubbed() {
        let scrubber = EnvScrubber::new(
            ["MY_PROVIDER_KEY".to_string()],
            ["sk-very-secret-value".to_string()],
        );
        let mut f = fx_with(ToolsConfig::default(), scrubber);
        let env = vec![
            ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
            ("ANTHROPIC_API_KEY".into(), "sk-ant-1".into()),
            ("MY_PROVIDER_KEY".into(), "x".into()),
            ("HARNESS_CONFIG".into(), "/secret/harness.ini".into()),
            ("LEAKY".into(), "Bearer sk-very-secret-value".into()),
            ("KEEP_ME".into(), "ok".into()),
            ("PAGER".into(), "less".into()),
        ];
        let tool = Arc::into_inner(f.tool).unwrap().with_base_env(env);
        f.tool = Arc::new(tool);
        let out = run(&f, "env | sort").await;
        assert!(out.contains("KEEP_ME=ok"), "{out}");
        assert!(out.contains("PAGER=cat"), "{out}");
        assert!(out.contains("GIT_TERMINAL_PROMPT=0"), "{out}");
        for bad in [
            "ANTHROPIC_API_KEY",
            "MY_PROVIDER_KEY",
            "HARNESS_CONFIG",
            "LEAKY",
            "sk-",
        ] {
            assert!(!out.contains(bad), "{bad} leaked: {out}");
        }
    }

    #[test]
    fn short_secret_values_are_ignored() {
        let s = EnvScrubber::new([], ["abc".to_string()]);
        assert!(!s.is_secret("X", "abcdef"));
        assert!(s.is_secret("OPENAI_API_KEY", "anything"));
    }

    #[tokio::test]
    async fn working_directory_persists() {
        let f = fx();
        std::fs::create_dir(f.dir.path().join("sub")).unwrap();
        run(&f, "cd sub").await;
        let want = std::fs::canonicalize(f.dir.path().join("sub")).unwrap();
        assert_eq!(std::fs::canonicalize(f.tool.cwd()).unwrap(), want);
        assert_eq!(run(&f, "pwd -P").await.trim_end(), want.to_string_lossy());
        // cd then a failing exit still records the new directory.
        run(&f, "cd ..; exit 3").await;
        assert_eq!(
            std::fs::canonicalize(f.tool.cwd()).unwrap(),
            std::fs::canonicalize(f.dir.path()).unwrap()
        );
        // A deleted directory falls back with a note instead of failing silently.
        run(&f, "mkdir gone && cd gone").await;
        std::fs::remove_dir(f.dir.path().join("gone")).unwrap();
        let out = run(&f, "echo still running").await;
        assert!(out.contains("no longer exists"), "{out}");
        assert!(out.contains("still running"), "{out}");
    }

    #[test]
    fn marker_filter_handles_split_markers() {
        let marker = "__M__:";
        let mut f = MarkerFilter::new(marker);
        let mut out = Vec::new();
        for piece in ["abc\n", "de", "f__", "M_", "_:/tmp/x", "\n"] {
            out.extend(f.feed(piece.as_bytes()));
        }
        let (rest, cwd) = f.finish();
        out.extend(rest);
        assert_eq!(String::from_utf8(out).unwrap(), "abc\ndef");
        assert_eq!(cwd.as_deref(), Some("/tmp/x"));
        let mut f = MarkerFilter::new(marker);
        let mut out = f.feed(b"a\n_");
        assert_eq!(out, b"a\n", "partial marker prefix is held back");
        let (rest, cwd) = f.finish();
        out.extend(rest);
        assert_eq!(out, b"a\n_");
        assert_eq!(cwd, None);
    }

    #[test]
    fn capture_keeps_head_and_tail() {
        let mut c = Capture::default();
        let block = vec![b'a'; CAPTURE_HALF];
        c.push(&block);
        c.push(&vec![b'b'; CAPTURE_HALF * 2]);
        c.push(b"END");
        let s = c.into_string();
        assert!(s.starts_with('a'));
        assert!(s.ends_with("END"));
        assert!(s.contains("bytes of output discarded"));
    }
}
