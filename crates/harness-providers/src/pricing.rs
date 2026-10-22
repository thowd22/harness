//! Token pricing and per-call cost calculation.
//!
//! Prices are in USD per million tokens, the unit used by `[pricing]` in
//! `harness.ini` and by provider price lists. A [`Price`] turns a
//! [`Usage`] record into a dollar amount with [`Price::cost`].

use serde::{Deserialize, Serialize};

use crate::model::Usage;

/// Cache reads cost this fraction of the input price when a price does not
/// set `cache_read` explicitly.
pub const DEFAULT_CACHE_READ_FACTOR: f64 = 0.1;

/// Cache writes cost this multiple of the input price when a price does not
/// set `cache_write` explicitly (the 5-minute cache write premium).
pub const DEFAULT_CACHE_WRITE_FACTOR: f64 = 1.25;

const PER_MILLION: f64 = 1_000_000.0;

/// Prices for one model, in USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Price {
    /// Uncached input tokens.
    pub input: f64,
    /// Output tokens (reasoning included).
    pub output: f64,
    /// Input tokens read from the prompt cache. `None` means
    /// [`DEFAULT_CACHE_READ_FACTOR`] times `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    /// Input tokens written to the prompt cache. `None` means
    /// [`DEFAULT_CACHE_WRITE_FACTOR`] times `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

impl Price {
    /// A price with the given input and output rates and default cache rates.
    pub const fn new(input: f64, output: f64) -> Self {
        Self {
            input,
            output,
            cache_read: None,
            cache_write: None,
        }
    }

    /// A price with explicit cache read and write rates.
    pub const fn with_cache(input: f64, output: f64, cache_read: f64, cache_write: f64) -> Self {
        Self {
            input,
            output,
            cache_read: Some(cache_read),
            cache_write: Some(cache_write),
        }
    }

    /// The effective cache read rate.
    pub fn cache_read_rate(&self) -> f64 {
        self.cache_read
            .unwrap_or(self.input * DEFAULT_CACHE_READ_FACTOR)
    }

    /// The effective cache write rate.
    pub fn cache_write_rate(&self) -> f64 {
        self.cache_write
            .unwrap_or(self.input * DEFAULT_CACHE_WRITE_FACTOR)
    }

    /// Cost of one call in USD.
    ///
    /// Reasoning tokens are part of `output_tokens`, so they are not charged
    /// twice.
    pub fn cost(&self, usage: &Usage) -> f64 {
        let tokens = |n: u64| n as f64 / PER_MILLION;
        tokens(usage.input_tokens) * self.input
            + tokens(usage.output_tokens) * self.output
            + tokens(usage.cache_read_tokens) * self.cache_read_rate()
            + tokens(usage.cache_write_tokens) * self.cache_write_rate()
    }
}

/// Cost of a call when the price may be unknown. `None` means the cost is
/// unknown and must be shown as such, never as zero.
pub fn cost(price: Option<&Price>, usage: &Usage) -> Option<f64> {
    price.map(|p| p.cost(usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64, read: u64, write: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: read,
            cache_write_tokens: write,
            reasoning_tokens: 0,
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn cost_charges_every_counter_at_its_rate() {
        let p = Price::with_cache(3.0, 15.0, 0.3, 3.75);
        let c = p.cost(&usage(1_000_000, 1_000_000, 1_000_000, 1_000_000));
        assert!(close(c, 3.0 + 15.0 + 0.3 + 3.75), "{c}");
        assert!(close(p.cost(&usage(2_000, 500, 0, 0)), 0.006 + 0.0075));
    }

    #[test]
    fn missing_cache_rates_use_default_factors() {
        let p = Price::new(4.0, 20.0);
        assert!(close(p.cache_read_rate(), 0.4));
        assert!(close(p.cache_write_rate(), 5.0));
    }

    #[test]
    fn reasoning_is_not_double_counted() {
        let p = Price::new(1.0, 10.0);
        let mut u = usage(0, 1_000_000, 0, 0);
        u.reasoning_tokens = 600_000;
        assert!(close(p.cost(&u), 10.0));
    }

    #[test]
    fn unknown_price_gives_unknown_cost() {
        assert_eq!(cost(None, &usage(1, 1, 0, 0)), None);
        assert!(cost(Some(&Price::new(1.0, 1.0)), &usage(1, 1, 0, 0)).is_some());
    }
}
