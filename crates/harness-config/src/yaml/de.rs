//! Small serde helpers shared by the YAML schemas.

use std::fmt;

use serde::Deserializer;
use serde::de::{Error, Visitor};

struct LenientU64;

impl<'de> Visitor<'de> for LenientU64 {
    type Value = u64;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a non-negative integer such as 2000000 or 2_000_000")
    }

    fn visit_u64<E: Error>(self, v: u64) -> Result<u64, E> {
        Ok(v)
    }

    fn visit_i64<E: Error>(self, v: i64) -> Result<u64, E> {
        u64::try_from(v).map_err(|_| E::custom(format!("expected a non-negative integer, got {v}")))
    }

    fn visit_str<E: Error>(self, s: &str) -> Result<u64, E> {
        let ok = !s.is_empty()
            && !s.starts_with('_')
            && !s.ends_with('_')
            && s.chars().all(|c| c.is_ascii_digit() || c == '_');
        if !ok {
            return Err(E::custom(format!(
                "invalid number `{s}`: expected a non-negative integer such as 2000000 or 2_000_000"
            )));
        }
        let digits: String = s.chars().filter(|c| *c != '_').collect();
        digits
            .parse()
            .map_err(|_| E::custom(format!("number `{s}` is too large")))
    }
}

struct OptVisitor;

impl<'de> Visitor<'de> for OptVisitor {
    type Value = Option<u64>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a non-negative integer or nothing")
    }

    fn visit_none<E: Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E: Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        lenient_u64(d).map(Some)
    }
}

/// Deserializes a `u64` that may use `_` digit separators (`2_000_000`).
pub fn lenient_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    d.deserialize_any(LenientU64)
}

/// Optional form of [`lenient_u64`]; use with `#[serde(default)]`.
pub fn lenient_opt_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    d.deserialize_option(OptVisitor)
}

/// Optional `u32` with `_` separators allowed; use with `#[serde(default)]`.
pub fn lenient_opt_u32<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    match lenient_opt_u64(d)? {
        None => Ok(None),
        Some(n) => u32::try_from(n)
            .map(Some)
            .map_err(|_| D::Error::custom(format!("number {n} is too large"))),
    }
}
