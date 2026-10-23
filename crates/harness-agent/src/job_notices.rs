//! Tells the agent when a background bash job has ended.
//!
//! `bash` with `run_in_background` returns at once (see
//! [`harness_tools::jobs`]). [`JobNotices`] is a [`RequestHook`]: before each
//! request it takes the jobs that ended since the last one and appends one
//! short notice per job (command, exit status, run time, the last lines of
//! output and the log path) to the newest message, like the todo nudge. That
//! message is a user prompt or tool results that has not been sent yet, so
//! earlier requests (and their prompt cache) stay unchanged and the notice
//! is recorded with the request that carries it. Process output is redacted
//! before the model sees it.
//!
//! Every agent loop of a session has one ([`JobNotices::agent`] for
//! subagents and workflow agent steps, [`JobNotices::session`] for the
//! top-level agent): an agent hears about the jobs it started, and the
//! top-level agent also about jobs whose agent has ended.

use async_trait::async_trait;
use harness_providers::model::{ContentBlock, Role};
use harness_tools::jobs::{JobInfo, JobManager, fmt_duration};

use crate::agent_loop::{HookDecision, RequestContext, RequestHook};
use crate::dispatch::ToolSet;

/// Lines of output a notice carries at most.
pub const NOTICE_TAIL_LINES: usize = 20;
/// Bytes of output a notice carries at most.
pub const NOTICE_TAIL_BYTES: usize = 2000;

/// Request hook that reports ended background jobs (see the module docs).
#[derive(Debug, Clone)]
pub struct JobNotices {
    jobs: JobManager,
    /// `None`: every job; else the owner and whether to take orphans.
    owner: Option<(u64, bool)>,
}

impl JobNotices {
    /// Reports every job of `jobs`.
    pub fn new(jobs: JobManager) -> Self {
        Self { jobs, owner: None }
    }

    /// Reports the jobs started by the bash tool of `tools` (a subagent or
    /// workflow agent step), if it has bash.
    pub fn agent(tools: &ToolSet) -> Option<Self> {
        Self::owned(tools, false)
    }

    /// Reports the jobs started by the bash tool of `tools` (the top-level
    /// agent) and the jobs whose bash tool is gone (subagents that ended
    /// while their job ran), if it has bash.
    pub fn session(tools: &ToolSet) -> Option<Self> {
        Self::owned(tools, true)
    }

    fn owned(tools: &ToolSet, orphans: bool) -> Option<Self> {
        Some(Self {
            jobs: tools.jobs()?,
            owner: Some((tools.job_owner()?, orphans)),
        })
    }
}

/// The last [`NOTICE_TAIL_LINES`] lines (at most [`NOTICE_TAIL_BYTES`]) of
/// `output`, cut at a character boundary.
pub fn tail_lines(output: &str) -> &str {
    let trimmed = output.trim_end_matches('\n');
    let lines_start = trimmed
        .rmatch_indices('\n')
        .nth(NOTICE_TAIL_LINES - 1)
        .map_or(0, |(i, _)| i + 1);
    let mut start = lines_start.max(trimmed.len().saturating_sub(NOTICE_TAIL_BYTES));
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    &trimmed[start..]
}

/// The notice for one ended job.
pub fn notice(job: &JobInfo, output_tail: &str) -> String {
    let mut s = format!(
        "Notice from harness: background job {} ended ({}) after {}.\nCommand: {}\n",
        job.id,
        job.status.describe(),
        fmt_duration(job.elapsed),
        job.command
    );
    let tail = tail_lines(output_tail);
    if tail.is_empty() {
        s.push_str("It printed nothing.");
    } else {
        s.push_str(&format!("Last output:\n{tail}\n"));
        if let Some(p) = &job.output {
            s.push_str(&format!(
                "Full output ({} bytes): {}",
                job.bytes,
                p.display()
            ));
        }
    }
    s
}

#[async_trait]
impl RequestHook for JobNotices {
    fn name(&self) -> &str {
        "job-notices"
    }

    async fn before_request(&self, ctx: &mut RequestContext<'_>) -> HookDecision {
        let conv = &mut *ctx.conversation;
        // Only onto the unsent tail: a user prompt or tool results.
        if conv.messages.len() <= conv.unlogged_from() {
            return HookDecision::Continue;
        }
        match conv.messages.last() {
            Some(m) if m.role == Role::User => {}
            _ => return HookDecision::Continue,
        }
        let ended = match self.owner {
            None => self.jobs.take_finished(),
            Some((owner, orphans)) => self.jobs.take_finished_for(owner, orphans),
        };
        if ended.is_empty() {
            return HookDecision::Continue;
        }
        let redactor = ctx.recorder.redactor();
        let msg = conv.messages.last_mut().expect("checked above");
        for (job, tail) in &ended {
            let text = notice(job, tail);
            msg.content
                .push(ContentBlock::text(redactor.redact(&text).into_owned()));
        }
        HookDecision::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_last_lines_within_the_byte_budget() {
        let many: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let t = tail_lines(&many);
        assert!(t.starts_with("line 31\n"), "{t}");
        assert!(t.ends_with("line 50"), "{t}");
        assert_eq!(tail_lines("a\nb\n"), "a\nb");
        let wide = "é".repeat(3000);
        let t = tail_lines(&wide);
        assert!(t.len() <= NOTICE_TAIL_BYTES, "{}", t.len());
        assert!(t.chars().all(|c| c == 'é'));
        assert_eq!(tail_lines(""), "");
    }
}
