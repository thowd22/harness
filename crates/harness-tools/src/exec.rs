//! The [`Exec`] trait: every process a tool starts goes through it.
//!
//! A spawned process is represented by an [`ExecHandle`], a small
//! channel-backed struct, so local and remote implementations produce the
//! same type: the implementation feeds output chunks and the final
//! [`ExitOutcome`] into channels and listens for a kill request.
//!
//! Contract every implementation must honour (checked by
//! `crate::conformance::exec_suite`):
//! - stdin is closed (`/dev/null`), so interactive prompts fail fast;
//! - the process runs in its own process group, and timeout, [`ExecHandle::kill`]
//!   or dropping the handle kills the **whole group**;
//! - all output is delivered before the exit outcome.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

/// Which output stream a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// A piece of process output, delivered as soon as it is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputChunk {
    /// Stream the bytes came from.
    pub stream: StreamKind,
    /// Raw bytes (not necessarily valid UTF-8 or line-aligned).
    pub data: Vec<u8>,
}

/// What to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRequest {
    /// Program to execute (looked up on `PATH` if not absolute).
    pub program: String,
    /// Arguments, not including the program name.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Environment variables to set.
    pub env: Vec<(String, String)>,
    /// When true, start from an empty environment so only [`ExecRequest::env`] is visible.
    pub clear_env: bool,
    /// Kill the process group after this long. `None` means no limit.
    pub timeout: Option<Duration>,
}

impl ExecRequest {
    /// A request for `program` with `args` in `cwd`, inheriting the
    /// environment, with no timeout.
    pub fn new(program: impl Into<String>, args: Vec<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args,
            cwd: cwd.into(),
            env: Vec::new(),
            clear_env: false,
            timeout: None,
        }
    }

    /// Set the timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Replace the environment entirely with `env`.
    pub fn with_clean_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self.clear_env = true;
        self
    }
}

/// How a process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExitOutcome {
    /// Exit code, if the process exited normally.
    pub code: Option<i32>,
    /// Terminating signal number, if it was killed by a signal.
    pub signal: Option<i32>,
    /// True if the timeout fired and the group was killed.
    pub timed_out: bool,
    /// True if the group was killed because of [`ExecHandle::kill`] or drop.
    pub killed: bool,
}

impl ExitOutcome {
    /// True for exit code 0 with no timeout or kill.
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out && !self.killed
    }
}

/// Errors from starting or supervising a process.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// The process could not be started.
    #[error("failed to start {program}: {source}")]
    Spawn {
        /// Program that failed to start.
        program: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The implementation stopped without reporting an exit outcome
    /// (for example a lost remote connection).
    #[error("process supervision ended without an exit status")]
    Lost,
}

/// A running process.
///
/// Read output with [`ExecHandle::next_output`] until it returns `None`, then
/// call [`ExecHandle::wait`]. Dropping the handle before the process exits
/// kills its process group, so cancelling a tool call is just dropping its
/// future.
#[derive(Debug)]
pub struct ExecHandle {
    output: mpsc::UnboundedReceiver<OutputChunk>,
    kill: Option<oneshot::Sender<()>>,
    status: oneshot::Receiver<ExitOutcome>,
}

impl ExecHandle {
    /// Build a handle from the implementation's channels.
    ///
    /// The implementation must: send every chunk on `output` and then drop
    /// it; kill the process group when `kill` fires **or is dropped**; and
    /// send the outcome on `status` after the last chunk.
    pub fn new(
        output: mpsc::UnboundedReceiver<OutputChunk>,
        kill: oneshot::Sender<()>,
        status: oneshot::Receiver<ExitOutcome>,
    ) -> Self {
        Self {
            output,
            kill: Some(kill),
            status,
        }
    }

    /// Next output chunk, or `None` once both streams are closed.
    pub async fn next_output(&mut self) -> Option<OutputChunk> {
        self.output.recv().await
    }

    /// Ask the implementation to kill the whole process group. Idempotent.
    pub fn kill(&mut self) {
        if let Some(tx) = self.kill.take() {
            let _ = tx.send(());
        }
    }

    /// Wait for the exit outcome. Unread output is discarded.
    pub async fn wait(self) -> Result<ExitOutcome, ExecError> {
        let ExecHandle {
            mut output,
            kill,
            status,
        } = self;
        output.close();
        // Keep the kill sender alive while waiting so waiting does not kill.
        let outcome = status.await.map_err(|_| ExecError::Lost);
        drop(kill);
        outcome
    }

    /// Read all remaining output, calling `on_chunk` for each chunk, then
    /// wait for the outcome. Convenience for the common loop.
    pub async fn drive(
        mut self,
        mut on_chunk: impl FnMut(&OutputChunk),
    ) -> Result<ExitOutcome, ExecError> {
        while let Some(chunk) = self.next_output().await {
            on_chunk(&chunk);
        }
        self.wait().await
    }
}

/// Starts processes on one host.
#[async_trait]
pub trait Exec: Send + Sync {
    /// Start `req`. Output starts streaming immediately.
    async fn spawn(&self, req: ExecRequest) -> Result<ExecHandle, ExecError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_requires_zero_and_no_kill() {
        assert!(
            ExitOutcome {
                code: Some(0),
                ..Default::default()
            }
            .success()
        );
        assert!(
            !ExitOutcome {
                code: Some(0),
                timed_out: true,
                ..Default::default()
            }
            .success()
        );
        assert!(
            !ExitOutcome {
                code: Some(1),
                ..Default::default()
            }
            .success()
        );
    }

    #[tokio::test]
    async fn dropping_handle_signals_kill() {
        let (_otx, orx) = mpsc::unbounded_channel();
        let (ktx, krx) = oneshot::channel();
        let (_stx, srx) = oneshot::channel();
        drop(ExecHandle::new(orx, ktx, srx));
        // Sender dropped: the implementation's receiver resolves with Err.
        assert!(krx.await.is_err());
    }
}
