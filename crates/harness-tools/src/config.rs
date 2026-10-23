//! Limits for the core tools: the `[tools]` section of `harness.ini`.

use std::time::Duration;

use serde::Deserialize;

/// Tool limits. Field names are the `[tools]` keys in `harness.ini`; every
/// key is optional and falls back to [`ToolsConfig::default`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsConfig {
    /// Most lines one read call returns (`read_max_lines`, default 2000).
    pub read_max_lines: usize,
    /// Most bytes one read call returns (`read_max_bytes`, default 32000).
    pub read_max_bytes: usize,
    /// Longer lines are cut in read output (`read_max_line_chars`, default 2000).
    pub read_max_line_chars: usize,
    /// Files larger than this are refused by read (`read_max_file_bytes`, default 64 MiB).
    pub read_max_file_bytes: u64,
    /// bash timeout when the call gives none, seconds (`bash_timeout`, default 120).
    pub bash_timeout: u64,
    /// Largest timeout a bash call may ask for, seconds (`bash_max_timeout`, default 600).
    pub bash_max_timeout: u64,
    /// Outputs larger than this many bytes are spilled to a blob (`spill_over_bytes`, default 16000).
    pub spill_over_bytes: usize,
    /// Lines kept from the start of a spilled output (`spill_head_lines`, default 40).
    pub spill_head_lines: usize,
    /// Lines kept from the end of a spilled output (`spill_tail_lines`, default 40).
    pub spill_tail_lines: usize,
    /// Background bash jobs that may run at once (`bash_max_jobs`, default 4).
    pub bash_max_jobs: usize,
    /// Output bytes written to a background job's log (`bash_job_output_bytes`,
    /// default 32 MiB); later output is dropped.
    pub bash_job_output_bytes: u64,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            read_max_lines: 2000,
            read_max_bytes: 32_000,
            read_max_line_chars: 2000,
            read_max_file_bytes: 64 * 1024 * 1024,
            bash_timeout: 120,
            bash_max_timeout: 600,
            spill_over_bytes: 16_000,
            spill_head_lines: 40,
            spill_tail_lines: 40,
            bash_max_jobs: 4,
            bash_job_output_bytes: 32 * 1024 * 1024,
        }
    }
}

impl ToolsConfig {
    /// Default bash timeout as a [`Duration`].
    pub fn bash_timeout(&self) -> Duration {
        Duration::from_secs(self.bash_timeout)
    }

    /// Maximum bash timeout as a [`Duration`].
    pub fn bash_max_timeout(&self) -> Duration {
        Duration::from_secs(self.bash_max_timeout.max(self.bash_timeout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_config_keeps_defaults() {
        let c: ToolsConfig = serde_json::from_str(r#"{"bash_timeout": 5}"#).unwrap();
        assert_eq!(c.bash_timeout, 5);
        assert_eq!(c.read_max_lines, 2000);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(serde_json::from_str::<ToolsConfig>(r#"{"bash_timout": 5}"#).is_err());
    }

    #[test]
    fn max_timeout_never_below_default() {
        let c = ToolsConfig {
            bash_timeout: 900,
            ..Default::default()
        };
        assert_eq!(c.bash_max_timeout(), Duration::from_secs(900));
    }
}
