//! Feeds the [`LongWait`] cache-lifetime hint from the agent loop.
//!
//! [`LongWaitHook`] is a [`RequestHook`]: before each request it reports
//! whether any background job of the session is running and the
//! start-to-start gap since the previous request of the same agent run (a
//! long gap means a long tool round). A provider configured with
//! `[provider.anthropic] cache_ttl_auto_1h` then writes one-hour cache
//! entries while the hint is active (see
//! [`harness_providers::cache_hint`]). The hook never changes the
//! conversation, so requests are byte-identical with or without it.

use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;
use harness_events::schema::NodeId;
use harness_providers::cache_hint::LongWait;
use harness_tools::jobs::JobManager;

use crate::agent_loop::{HookDecision, RequestContext, RequestHook};

/// Request hook that reports long waits (see the module docs).
#[derive(Debug)]
pub struct LongWaitHook {
    hint: LongWait,
    jobs: Option<JobManager>,
    last: Mutex<Option<(NodeId, Instant)>>,
}

impl LongWaitHook {
    /// Reports to the process-wide hint; `jobs` is the session's
    /// background-job manager, if the agent has one.
    pub fn new(jobs: Option<JobManager>) -> Self {
        Self::with_hint(LongWait::process(), jobs)
    }

    /// Reports to `hint` instead (tests).
    pub fn with_hint(hint: LongWait, jobs: Option<JobManager>) -> Self {
        Self {
            hint,
            jobs,
            last: Mutex::new(None),
        }
    }
}

#[async_trait]
impl RequestHook for LongWaitHook {
    fn name(&self) -> &str {
        "long-wait"
    }

    async fn before_request(&self, ctx: &mut RequestContext<'_>) -> HookDecision {
        if let Some(jobs) = &self.jobs {
            self.hint.set_jobs_running(jobs.running() > 0);
        }
        let now = Instant::now();
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        // Only gaps inside one run count: the pause before a new user
        // prompt is the user's, not a tool's.
        if ctx.turn > 0
            && let Some((node, at)) = last.as_ref()
            && node == ctx.node
        {
            self.hint.note_gap(now.duration_since(*at));
        }
        *last = Some((ctx.node.clone(), now));
        HookDecision::Continue
    }
}
