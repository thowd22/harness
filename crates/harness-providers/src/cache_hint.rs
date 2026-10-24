//! A process-wide hint that the next gap between requests may be long.
//!
//! Prompt-cache entries written with the default five-minute lifetime
//! expire when the next request that shares their prefix starts more than
//! five minutes later (the lifetime counts from the start of the request
//! that wrote or last read them). Two situations make such gaps likely:
//!
//! - a background job is running (the agent may end its turn and wait for
//!   the job's notice), and
//! - this session recently had a long start-to-start gap between two
//!   requests of one agent run (a long tool round: a test suite, a build,
//!   a subagent fan-out), which tends to repeat.
//!
//! The agent side reports both ([`LongWait::set_jobs_running`],
//! [`LongWait::note_gap`]); a provider configured for it
//! (`[provider.anthropic] cache_ttl_auto_1h`) asks [`LongWait::active`]
//! when it builds a request and then writes one-hour entries instead.
//! The hint only changes cache lifetimes, never what is sent to the model.
//!
//! The signal is process-wide ([`LongWait::process`]), like the gzip
//! latch: one harness process normally runs one session. Tests use
//! [`LongWait::new`] for an isolated one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A start-to-start gap between two requests of one agent run at least
/// this long counts as a long tool round.
pub const LONG_GAP: Duration = Duration::from_secs(3 * 60);

/// How long a long gap keeps the hint active.
pub const LONG_GAP_WINDOW: Duration = Duration::from_secs(30 * 60);

#[derive(Debug)]
struct Inner {
    long_gap: Duration,
    jobs_running: AtomicBool,
    last_long_gap: Mutex<Option<Instant>>,
}

/// The shared hint (see the module docs). Clones share state.
#[derive(Debug, Clone)]
pub struct LongWait(Arc<Inner>);

impl Default for LongWait {
    fn default() -> Self {
        Self::with_long_gap(LONG_GAP)
    }
}

impl PartialEq for LongWait {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl LongWait {
    /// A new, inactive hint that shares nothing with others.
    pub fn new() -> Self {
        Self::default()
    }

    /// A new hint that counts gaps of `long_gap` or more as long (tests).
    pub fn with_long_gap(long_gap: Duration) -> Self {
        Self(Arc::new(Inner {
            long_gap,
            jobs_running: AtomicBool::new(false),
            last_long_gap: Mutex::new(None),
        }))
    }

    /// The process-wide hint.
    pub fn process() -> Self {
        static HINT: OnceLock<LongWait> = OnceLock::new();
        HINT.get_or_init(LongWait::new).clone()
    }

    /// Records whether any background job of the session is running.
    pub fn set_jobs_running(&self, running: bool) {
        self.0.jobs_running.store(running, Ordering::Relaxed);
    }

    /// Records the start-to-start gap between two requests of one agent
    /// run; gaps of [`LONG_GAP`] (or the hint's own threshold) or more
    /// activate the hint for [`LONG_GAP_WINDOW`].
    pub fn note_gap(&self, gap: Duration) {
        if gap >= self.0.long_gap {
            *self
                .0
                .last_long_gap
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        }
    }

    /// True while a background job runs or within [`LONG_GAP_WINDOW`] of
    /// a long gap.
    pub fn active(&self) -> bool {
        self.0.jobs_running.load(Ordering::Relaxed)
            || self
                .0
                .last_long_gap
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some_and(|at| at.elapsed() < LONG_GAP_WINDOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_and_long_gaps_activate_the_hint() {
        let h = LongWait::new();
        assert!(!h.active());
        h.set_jobs_running(true);
        assert!(h.active());
        h.set_jobs_running(false);
        assert!(!h.active());
        h.note_gap(LONG_GAP - Duration::from_secs(1));
        assert!(!h.active(), "short gaps do not count");
        h.note_gap(LONG_GAP);
        assert!(h.active());
        assert!(!LongWait::new().active(), "separate hints share nothing");
        assert_eq!(LongWait::process(), LongWait::process());
        assert_ne!(LongWait::process(), h);
    }
}
