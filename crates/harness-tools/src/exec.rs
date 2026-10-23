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
//!
//! An implementation may also report [`Liveness`] samples of the process
//! group ([`ExecHandle::with_liveness`]); callers read them through
//! [`ExecHandle::next_event`]. Implementations that cannot sample simply
//! attach nothing.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

pub use crate::liveness::Liveness;

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

/// Something a running process reported: output or a liveness sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecEvent {
    /// A piece of output.
    Output(OutputChunk),
    /// A periodic liveness sample of the process group.
    Liveness(Liveness),
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
    liveness: Option<mpsc::UnboundedReceiver<Liveness>>,
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
            liveness: None,
        }
    }

    /// Attach a channel of [`Liveness`] samples (sent at a bounded
    /// interval while the process runs). Without one, [`Self::next_event`]
    /// yields output only and liveness is unknown.
    pub fn with_liveness(mut self, liveness: mpsc::UnboundedReceiver<Liveness>) -> Self {
        self.liveness = Some(liveness);
        self
    }

    /// Next output chunk or liveness sample, or `None` once both output
    /// streams are closed. Output is preferred when both are ready.
    pub async fn next_event(&mut self) -> Option<ExecEvent> {
        loop {
            let Some(live) = self.liveness.as_mut() else {
                return self.output.recv().await.map(ExecEvent::Output);
            };
            let sample = tokio::select! {
                biased;
                chunk = self.output.recv() => return chunk.map(ExecEvent::Output),
                sample = live.recv() => sample,
            };
            match sample {
                Some(s) => return Some(ExecEvent::Liveness(s)),
                // The sampler stopped: output only from now on.
                None => self.liveness = None,
            }
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
            liveness,
        } = self;
        output.close();
        drop(liveness);
        // Keep the kill sender alive while waiting so waiting does not kill.
        let outcome = status.await.map_err(|_| ExecError::Lost);
        drop(kill);
        outcome
    }

    /// Like [`Self::wait`], but kill the process group as soon as `kill`
    /// completes (with any value), so a wrapper that relays this handle can
    /// still kill after the output streams have closed.
    pub async fn wait_or_kill(
        self,
        kill: impl std::future::Future<Output = impl Sized>,
    ) -> Result<ExitOutcome, ExecError> {
        let ExecHandle {
            mut output,
            kill: mut kill_tx,
            mut status,
            liveness,
        } = self;
        output.close();
        drop(liveness);
        tokio::pin!(kill);
        let mut armed = true;
        let outcome = loop {
            tokio::select! {
                outcome = &mut status => break outcome.map_err(|_| ExecError::Lost),
                _ = &mut kill, if armed => {
                    armed = false;
                    if let Some(tx) = kill_tx.take() {
                        let _ = tx.send(());
                    }
                }
            }
        };
        drop(kill_tx);
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
    async fn next_event_interleaves_liveness_and_survives_a_stopped_sampler() {
        let (otx, orx) = mpsc::unbounded_channel();
        let (ktx, _krx) = oneshot::channel();
        let (_stx, srx) = oneshot::channel();
        let (ltx, lrx) = mpsc::unbounded_channel();
        let mut h = ExecHandle::new(orx, ktx, srx).with_liveness(lrx);
        let sample = Liveness {
            cpu: Duration::from_millis(40),
            procs: 2,
            active: 1,
        };
        ltx.send(sample).unwrap();
        assert_eq!(h.next_event().await, Some(ExecEvent::Liveness(sample)));
        drop(ltx);
        let chunk = OutputChunk {
            stream: StreamKind::Stdout,
            data: b"hi".to_vec(),
        };
        otx.send(chunk.clone()).unwrap();
        drop(otx);
        assert_eq!(h.next_event().await, Some(ExecEvent::Output(chunk)));
        assert_eq!(h.next_event().await, None);
        // Without a liveness channel it is plain output.
        let (_otx, orx) = mpsc::unbounded_channel::<OutputChunk>();
        let (ktx, _krx) = oneshot::channel();
        let (_stx, srx) = oneshot::channel();
        let mut h = ExecHandle::new(orx, ktx, srx);
        drop(_otx);
        assert_eq!(h.next_event().await, None);
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

    #[tokio::test]
    async fn wait_or_kill_kills_on_request_and_otherwise_just_waits() {
        // `kill` completes: the implementation sees a kill, then reports.
        let (_otx, orx) = mpsc::unbounded_channel::<OutputChunk>();
        let (ktx, krx) = oneshot::channel();
        let (stx, srx) = oneshot::channel();
        let (req_tx, req_rx) = oneshot::channel::<()>();
        let waiter = tokio::spawn(ExecHandle::new(orx, ktx, srx).wait_or_kill(req_rx));
        req_tx.send(()).unwrap();
        assert!(krx.await.is_ok(), "kill sent");
        let killed = ExitOutcome {
            killed: true,
            ..Default::default()
        };
        stx.send(killed).unwrap();
        assert_eq!(waiter.await.unwrap().unwrap(), killed);

        // `kill` never completes: no kill while waiting, like `wait`.
        let (_otx, orx) = mpsc::unbounded_channel::<OutputChunk>();
        let (ktx, mut krx) = oneshot::channel();
        let (stx, srx) = oneshot::channel();
        let waiter =
            tokio::spawn(ExecHandle::new(orx, ktx, srx).wait_or_kill(std::future::pending::<()>()));
        tokio::task::yield_now().await;
        assert!(
            krx.try_recv()
                .is_err_and(|e| e == oneshot::error::TryRecvError::Empty)
        );
        stx.send(ExitOutcome::default()).unwrap();
        assert!(waiter.await.unwrap().is_ok());
    }
}
