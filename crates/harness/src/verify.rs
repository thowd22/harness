//! The done-gating verifier: runs the verify command in the project.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use harness_agent::agent_loop::{Verifier, VerifyResult};
use harness_tools::exec::{Exec, ExecRequest};
use tokio_util::sync::CancellationToken;

/// Runs `[verify] command` (or the agent's override) with `sh -c` in the
/// project, with secrets scrubbed from the environment.
pub struct ShellVerifier {
    command: String,
    cwd: std::path::PathBuf,
    exec: Arc<dyn Exec>,
    env: Vec<(String, String)>,
}

impl ShellVerifier {
    /// Runs `command` in `cwd` on `exec` with exactly `env`.
    pub fn new(
        command: String,
        cwd: std::path::PathBuf,
        exec: Arc<dyn Exec>,
        env: Vec<(String, String)>,
    ) -> Self {
        Self {
            command,
            cwd,
            exec,
            env,
        }
    }
}

/// Longest verify output kept, in bytes.
const VERIFY_OUTPUT_MAX: usize = 16_000;

#[async_trait]
impl Verifier for ShellVerifier {
    fn command(&self) -> String {
        self.command.clone()
    }

    async fn verify(&self, cancel: &CancellationToken) -> VerifyResult {
        let req = ExecRequest::new("sh", vec!["-c".into(), self.command.clone()], &self.cwd)
            .with_clean_env(self.env.clone())
            .with_timeout(Duration::from_secs(1800));
        let handle = match self.exec.spawn(req).await {
            Ok(h) => h,
            Err(e) => {
                return VerifyResult {
                    passed: false,
                    exit_code: None,
                    output: format!("could not start verify command: {e}"),
                };
            }
        };
        let mut output = Vec::new();
        let run = handle.drive(|chunk| {
            if output.len() < VERIFY_OUTPUT_MAX {
                output.extend_from_slice(&chunk.data);
            }
        });
        let outcome = tokio::select! {
            r = run => r,
            _ = cancel.cancelled() => {
                return VerifyResult { passed: false, exit_code: None, output: "verification cancelled".into() };
            }
        };
        output.truncate(VERIFY_OUTPUT_MAX);
        let output = String::from_utf8_lossy(&output).into_owned();
        match outcome {
            Ok(exit) => VerifyResult {
                passed: exit.success(),
                exit_code: exit.code,
                output,
            },
            Err(e) => VerifyResult {
                passed: false,
                exit_code: None,
                output: format!("{output}\nverify command failed: {e}"),
            },
        }
    }
}
