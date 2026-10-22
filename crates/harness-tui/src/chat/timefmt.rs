//! Timestamps for transcript entries (`[ui] timestamps = time | relative | off`).
//!
//! Every time shown comes from an event's own `ts` field, so a live session
//! and a replay of its event log render the same `time` stamps. The UTC
//! offset is fixed when the view is built (detecting the local offset is
//! only sound before threads start), so it does not change mid-session.

pub use harness_config::ini::config::Timestamps;
use time::{OffsetDateTime, UtcOffset};

/// How timestamps are rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeFormat {
    /// Style from `[ui] timestamps`.
    pub mode: Timestamps,
    /// Offset that wall-clock times are shown in.
    pub offset: UtcOffset,
}

impl Default for TimeFormat {
    fn default() -> Self {
        TimeFormat {
            mode: Timestamps::Time,
            offset: UtcOffset::UTC,
        }
    }
}

impl TimeFormat {
    /// A format with the given mode and offset.
    pub fn new(mode: Timestamps, offset: UtcOffset) -> TimeFormat {
        TimeFormat { mode, offset }
    }

    /// The label for `ts`, or `None` when timestamps are off. `now` is only
    /// used by the relative style.
    pub fn format(&self, ts: OffsetDateTime, now: OffsetDateTime) -> Option<String> {
        match self.mode {
            Timestamps::Off => None,
            Timestamps::Time => {
                let t = ts.to_offset(self.offset);
                Some(format!(
                    "{:02}:{:02}:{:02}",
                    t.hour(),
                    t.minute(),
                    t.second()
                ))
            }
            Timestamps::Relative => Some(relative(now - ts)),
        }
    }
}

fn relative(d: time::Duration) -> String {
    let s = d.whole_seconds();
    if s < 5 {
        "now".to_owned()
    } else if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86_400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{datetime, offset};

    #[test]
    fn time_relative_and_off() {
        let ts = datetime!(2026-10-22 14:32:07 UTC);
        let now = datetime!(2026-10-22 14:37:10 UTC);
        let f = TimeFormat::default();
        assert_eq!(f.format(ts, now).as_deref(), Some("14:32:07"));
        let f = TimeFormat::new(Timestamps::Time, offset!(+2));
        assert_eq!(f.format(ts, now).as_deref(), Some("16:32:07"));
        let f = TimeFormat::new(Timestamps::Relative, UtcOffset::UTC);
        assert_eq!(f.format(ts, now).as_deref(), Some("5m ago"));
        assert_eq!(f.format(now, now).as_deref(), Some("now"));
        assert_eq!(
            f.format(ts, datetime!(2026-10-22 14:32:40 UTC)).as_deref(),
            Some("33s ago")
        );
        assert_eq!(
            f.format(ts, datetime!(2026-10-24 15:00:00 UTC)).as_deref(),
            Some("2d ago")
        );
        let f = TimeFormat::new(Timestamps::Off, UtcOffset::UTC);
        assert_eq!(f.format(ts, now), None);
    }
}
