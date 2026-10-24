//! Request-hook timing for `harness bench` (see
//! [`harness_providers::timing`] and `docs/performance.md`).
//!
//! [`wrap_hook`] returns the hook itself unless the timing probe is
//! installed, so the normal path is unchanged.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use harness_providers::timing::{self, Probe};

use crate::agent_loop::{CallReport, HookDecision, RequestContext, RequestHook};

/// Span name prefix of hook timings (`hook:<name>`).
pub const HOOK_SPAN: &str = "hook:";

/// `hook`, timed into the probe when it is installed.
pub fn wrap_hook(hook: Arc<dyn RequestHook>) -> Arc<dyn RequestHook> {
    match timing::active() {
        Some(probe) => Arc::new(TimedHook {
            span: format!("{HOOK_SPAN}{}", hook.name()),
            inner: hook,
            probe: probe.clone(),
        }),
        None => hook,
    }
}

/// A request hook whose `before_request` is recorded as a span.
struct TimedHook {
    inner: Arc<dyn RequestHook>,
    probe: Arc<Probe>,
    span: String,
}

#[async_trait]
impl RequestHook for TimedHook {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn before_request(&self, ctx: &mut RequestContext<'_>) -> HookDecision {
        let start = Instant::now();
        let decision = self.inner.before_request(ctx).await;
        self.probe.span(self.span.as_str(), start);
        decision
    }

    fn after_call(&self, report: &CallReport) {
        self.inner.after_call(report);
    }
}
