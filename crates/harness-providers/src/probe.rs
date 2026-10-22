//! Opt-in probe of request-body compression support (TASK-128).
//!
//! Anthropic accepts `Content-Encoding: gzip` request bodies (see
//! [`crate::http::compression`]); whether OpenAI and OpenRouter do is not
//! documented. This module measures it instead of guessing: it sends the
//! same tiny request (a token count or a one-token completion) once raw and
//! once per [`Encoding`], and classifies each answer.
//!
//! A server that ignores `Content-Encoding` tries to parse the compressed
//! bytes as JSON and fails, so only a success status proves the body was
//! decoded. The raw request is the control: if it does not succeed (bad
//! key, rate limit, outage), nothing can be concluded about the encodings.
//!
//! Nothing here runs by default. The live probe is the ignored test in
//! `tests/compression_probe.rs`; the findings and the decision per provider
//! are recorded in `docs/providers.md`.

use std::fmt;
use std::io::Write;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use crate::http::compression::{DEFAULT_GZIP_LEVEL, gzip};
use crate::http::error_message;
use crate::provider::ProviderError;

/// zstd level used by the probe (the library default).
pub const ZSTD_LEVEL: i32 = 3;

/// OpenAI's input-token count endpoint, relative to `/v1`. It bills
/// nothing, so it is the probe request for OpenAI.
pub const OPENAI_COUNT_PATH: &str = "responses/input_tokens";

/// Model named in the OpenAI count request.
pub const OPENAI_PROBE_MODEL: &str = "gpt-5";

/// Cheap OpenRouter model for the one-token completion (OpenRouter has no
/// count endpoint). Overridable with [`OPENROUTER_MODEL_ENV`].
pub const OPENROUTER_PROBE_MODEL: &str = "mistralai/mistral-nemo";

/// Environment variable that overrides [`OPENROUTER_PROBE_MODEL`].
pub const OPENROUTER_MODEL_ENV: &str = "HARNESS_PROBE_OPENROUTER_MODEL";

/// A request-body encoding under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// Raw JSON, no `Content-Encoding` (the control request).
    Identity,
    /// `Content-Encoding: gzip`.
    Gzip,
    /// `Content-Encoding: zstd`.
    Zstd,
}

impl Encoding {
    /// Every encoding, control first.
    pub const ALL: [Encoding; 3] = [Encoding::Identity, Encoding::Gzip, Encoding::Zstd];

    /// The `Content-Encoding` value; `None` for [`Encoding::Identity`].
    pub fn header(self) -> Option<&'static str> {
        match self {
            Encoding::Identity => None,
            Encoding::Gzip => Some("gzip"),
            Encoding::Zstd => Some("zstd"),
        }
    }

    /// `raw` encoded this way.
    pub fn encode(self, raw: &[u8]) -> Vec<u8> {
        match self {
            Encoding::Identity => raw.to_vec(),
            Encoding::Gzip => gzip(raw, DEFAULT_GZIP_LEVEL),
            Encoding::Zstd => {
                let mut enc = zstd::stream::Encoder::new(Vec::new(), ZSTD_LEVEL)
                    .expect("zstd encoder into memory");
                enc.write_all(raw).expect("zstd into memory");
                enc.finish().expect("zstd into memory")
            }
        }
    }
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.header().unwrap_or("identity"))
    }
}

/// One provider endpoint and the small JSON request sent to it.
#[derive(Debug, Clone)]
pub struct ProbeTarget {
    /// Provider name, e.g. `openrouter`.
    pub provider: String,
    /// Full request URL.
    pub url: String,
    /// Extra headers (auth). Credential values are marked sensitive.
    pub headers: HeaderMap,
    /// The request body, identical for every encoding.
    pub body: Value,
}

impl ProbeTarget {
    /// A target for `provider` at `url` sending `body` with `api_key` as a
    /// sensitive `Authorization: Bearer` header.
    pub fn new(
        provider: impl Into<String>,
        url: impl Into<String>,
        api_key: &str,
        body: Value,
    ) -> Result<Self, ProviderError> {
        let mut auth = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| ProviderError::Auth("API key contains invalid characters".into()))?;
        auth.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, auth);
        Ok(Self {
            provider: provider.into(),
            url: url.into(),
            headers,
            body,
        })
    }

    /// OpenAI: an input-token count at [`OPENAI_COUNT_PATH`] under
    /// `base_url` (usually [`crate::openai::DEFAULT_BASE_URL`]).
    pub fn openai(api_key: &str, base_url: &str) -> Result<Self, ProviderError> {
        Self::new(
            crate::openai::PROVIDER_NAME,
            join(base_url, OPENAI_COUNT_PATH),
            api_key,
            json!({"model": OPENAI_PROBE_MODEL, "input": "ok"}),
        )
    }

    /// OpenRouter: a non-streaming one-token chat completion for `model`
    /// under `base_url` (usually [`crate::openrouter::DEFAULT_BASE_URL`]).
    pub fn openrouter(api_key: &str, base_url: &str, model: &str) -> Result<Self, ProviderError> {
        Self::new(
            crate::openrouter::PROVIDER_NAME,
            join(base_url, crate::openrouter::CHAT_PATH),
            api_key,
            json!({
                "model": model,
                "messages": [{"role": "user", "content": "Say ok"}],
                "max_tokens": 1,
                "stream": false,
            }),
        )
    }
}

fn join(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// How the server answered one encoded request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A success status: the server decoded the body.
    Accepted,
    /// A client error for an encoded body while the raw body succeeded:
    /// the encoding is not supported.
    Rejected {
        /// HTTP status.
        status: u16,
        /// Error message from the body.
        message: String,
    },
    /// Nothing can be concluded (control failed, auth, rate limit, server
    /// error or network failure).
    Inconclusive {
        /// HTTP status, when a response arrived.
        status: Option<u16>,
        /// Why.
        message: String,
    },
}

/// The answer to one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    /// Encoding sent.
    pub encoding: Encoding,
    /// Bytes on the wire.
    pub wire_bytes: usize,
    /// Classification.
    pub verdict: Verdict,
}

/// What to do with a provider's compression setting after a probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// At least one encoding was accepted: enable it behind the same
    /// fallback latch as Anthropic (a follow-up task).
    Enable(Vec<Encoding>),
    /// The control succeeded and every encoding was rejected.
    LeaveOff,
    /// The probe could not decide; keep compression off and probe again.
    Inconclusive,
}

/// Results for one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    /// Provider name.
    pub provider: String,
    /// Raw body size in bytes.
    pub raw_bytes: usize,
    /// One result per encoding, in the order probed; the control first.
    pub results: Vec<ProbeResult>,
}

impl ProbeReport {
    /// The result for `encoding`, if it was probed.
    pub fn result(&self, encoding: Encoding) -> Option<&ProbeResult> {
        self.results.iter().find(|r| r.encoding == encoding)
    }

    /// Encodings (other than identity) the server accepted.
    pub fn accepted(&self) -> Vec<Encoding> {
        self.results
            .iter()
            .filter(|r| r.encoding != Encoding::Identity && r.verdict == Verdict::Accepted)
            .map(|r| r.encoding)
            .collect()
    }

    /// The decision these results support.
    pub fn decision(&self) -> Decision {
        let accepted = self.accepted();
        if !accepted.is_empty() {
            return Decision::Enable(accepted);
        }
        let encoded = || {
            self.results
                .iter()
                .filter(|r| r.encoding != Encoding::Identity)
        };
        let control_ok = self
            .result(Encoding::Identity)
            .is_some_and(|r| r.verdict == Verdict::Accepted);
        if control_ok
            && encoded().count() > 0
            && encoded().all(|r| matches!(r.verdict, Verdict::Rejected { .. }))
        {
            Decision::LeaveOff
        } else {
            Decision::Inconclusive
        }
    }

    /// True when gzip was accepted, which calls for a follow-up task to
    /// enable it with the Anthropic fallback design.
    pub fn needs_follow_up(&self) -> bool {
        self.accepted().contains(&Encoding::Gzip)
    }

    /// A plain-text table of the results, safe to print (no credentials).
    pub fn render(&self) -> String {
        let mut out = format!(
            "provider {} (raw body {} bytes)\n",
            self.provider, self.raw_bytes
        );
        for r in &self.results {
            let verdict = match &r.verdict {
                Verdict::Accepted => "accepted".to_string(),
                Verdict::Rejected { status, message } => format!("rejected {status}: {message}"),
                Verdict::Inconclusive { status, message } => match status {
                    Some(s) => format!("inconclusive {s}: {message}"),
                    None => format!("inconclusive: {message}"),
                },
            };
            out.push_str(&format!(
                "  {:<8} {:>5} bytes  {verdict}\n",
                r.encoding.to_string(),
                r.wire_bytes
            ));
        }
        out.push_str(&format!("  decision: {:?}\n", self.decision()));
        out
    }
}

/// Classifies the answer to an encoded request given whether the control
/// (raw) request succeeded. `status` is `None` for a transport failure.
pub fn classify(control_ok: bool, status: Option<u16>, body: &str) -> Verdict {
    let message = if body.is_empty() {
        String::new()
    } else {
        error_message(body)
    };
    match status {
        Some(s) if (200..300).contains(&s) => Verdict::Accepted,
        Some(s) if control_ok && matches!(s, 400 | 403 | 411 | 413 | 415 | 422) => {
            Verdict::Rejected { status: s, message }
        }
        status => Verdict::Inconclusive {
            status,
            message: if control_ok || status.is_none() {
                message
            } else {
                format!("control request failed; {message}")
            },
        },
    }
}

/// Sends `target`'s body once per encoding in `encodings` (the identity
/// control is always sent first, even if not listed) and classifies each
/// answer. Requests are sequential with `timeout` each and never retried.
pub async fn run(target: &ProbeTarget, encodings: &[Encoding], timeout: Duration) -> ProbeReport {
    let raw = serde_json::to_vec(&target.body).expect("JSON value serializes");
    let mut order = vec![Encoding::Identity];
    order.extend(
        encodings
            .iter()
            .copied()
            .filter(|e| *e != Encoding::Identity),
    );
    order.dedup();

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("harness/", env!("CARGO_PKG_VERSION")))
        .build();
    let mut results = Vec::new();
    let mut control_ok = false;
    for encoding in order {
        let wire = encoding.encode(&raw);
        let wire_bytes = wire.len();
        let answer = match &client {
            Ok(client) => send(client, target, encoding, wire).await,
            Err(e) => Err(format!("building HTTP client: {e}")),
        };
        let verdict = match answer {
            Ok((status, body)) => {
                if encoding == Encoding::Identity {
                    control_ok = (200..300).contains(&status);
                    if control_ok {
                        Verdict::Accepted
                    } else {
                        classify(false, Some(status), &body)
                    }
                } else {
                    classify(control_ok, Some(status), &body)
                }
            }
            Err(message) => Verdict::Inconclusive {
                status: None,
                message,
            },
        };
        results.push(ProbeResult {
            encoding,
            wire_bytes,
            verdict,
        });
    }
    ProbeReport {
        provider: target.provider.clone(),
        raw_bytes: raw.len(),
        results,
    }
}

async fn send(
    client: &reqwest::Client,
    target: &ProbeTarget,
    encoding: Encoding,
    wire: Vec<u8>,
) -> Result<(u16, String), String> {
    let mut req = client
        .post(&target.url)
        .headers(target.headers.clone())
        .header(CONTENT_TYPE, "application/json");
    if let Some(value) = encoding.header() {
        req = req.header(CONTENT_ENCODING, value);
    }
    let response = req
        .body(wire)
        .send()
        .await
        .map_err(|e| format!("request failed: {}", e.without_url()))?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn encodings_round_trip() {
        let raw = br#"{"model":"m","input":"ok"}"#.repeat(20);
        assert_eq!(Encoding::Identity.encode(&raw), raw);
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&Encoding::Gzip.encode(&raw)[..])
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, raw);
        assert_eq!(
            zstd::decode_all(&Encoding::Zstd.encode(&raw)[..]).unwrap(),
            raw
        );
        assert_eq!(Encoding::Gzip.header(), Some("gzip"));
        assert_eq!(Encoding::Zstd.to_string(), "zstd");
        assert_eq!(Encoding::Identity.to_string(), "identity");
    }

    #[test]
    fn classification() {
        let parse = r#"{"error":{"message":"JSON parsing failed","code":400}}"#;
        assert_eq!(classify(true, Some(200), "{}"), Verdict::Accepted);
        assert_eq!(
            classify(true, Some(400), parse),
            Verdict::Rejected {
                status: 400,
                message: "JSON parsing failed".into()
            }
        );
        assert!(matches!(
            classify(true, Some(415), ""),
            Verdict::Rejected { status: 415, .. }
        ));
        // A failed control makes any client error meaningless.
        assert!(matches!(
            classify(false, Some(400), parse),
            Verdict::Inconclusive { status: Some(400), ref message } if message.starts_with("control")
        ));
        // Auth, rate limits and outages say nothing about the encoding.
        for s in [401, 429, 500, 503] {
            assert!(matches!(
                classify(true, Some(s), ""),
                Verdict::Inconclusive { .. }
            ));
        }
        assert!(matches!(
            classify(true, None, ""),
            Verdict::Inconclusive { status: None, .. }
        ));
    }

    fn report(verdicts: &[(Encoding, Verdict)]) -> ProbeReport {
        ProbeReport {
            provider: "p".into(),
            raw_bytes: 10,
            results: verdicts
                .iter()
                .map(|(e, v)| ProbeResult {
                    encoding: *e,
                    wire_bytes: 10,
                    verdict: v.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn decisions() {
        let rej = Verdict::Rejected {
            status: 400,
            message: "JSON parsing failed".into(),
        };
        let inc = Verdict::Inconclusive {
            status: Some(429),
            message: "slow down".into(),
        };
        let off = report(&[
            (Encoding::Identity, Verdict::Accepted),
            (Encoding::Gzip, rej.clone()),
            (Encoding::Zstd, rej.clone()),
        ]);
        assert_eq!(off.decision(), Decision::LeaveOff);
        assert!(off.accepted().is_empty());
        assert!(!off.needs_follow_up());

        let gz = report(&[
            (Encoding::Identity, Verdict::Accepted),
            (Encoding::Gzip, Verdict::Accepted),
            (Encoding::Zstd, rej.clone()),
        ]);
        assert_eq!(gz.decision(), Decision::Enable(vec![Encoding::Gzip]));
        assert!(gz.needs_follow_up());

        let zs = report(&[
            (Encoding::Identity, Verdict::Accepted),
            (Encoding::Gzip, rej.clone()),
            (Encoding::Zstd, Verdict::Accepted),
        ]);
        assert_eq!(zs.decision(), Decision::Enable(vec![Encoding::Zstd]));
        assert!(!zs.needs_follow_up());

        let partial = report(&[
            (Encoding::Identity, Verdict::Accepted),
            (Encoding::Gzip, rej),
            (Encoding::Zstd, inc.clone()),
        ]);
        assert_eq!(partial.decision(), Decision::Inconclusive);
        let failed_control = report(&[(Encoding::Identity, inc.clone()), (Encoding::Gzip, inc)]);
        assert_eq!(failed_control.decision(), Decision::Inconclusive);
        assert_eq!(
            report(&[(Encoding::Identity, Verdict::Accepted)]).decision(),
            Decision::Inconclusive
        );
    }

    #[test]
    fn targets_build_small_requests_with_sensitive_auth() {
        let t = ProbeTarget::openrouter("sk-secret", "https://openrouter.ai/api/v1/", "m").unwrap();
        assert_eq!(t.url, "https://openrouter.ai/api/v1/chat/completions");
        assert_eq!(t.body["max_tokens"], 1);
        assert_eq!(t.body["stream"], false);
        assert!(t.headers[AUTHORIZATION].is_sensitive());
        assert!(!format!("{t:?}").contains("sk-secret"));

        let t = ProbeTarget::openai("sk-secret", "https://api.openai.com/v1").unwrap();
        assert_eq!(t.url, "https://api.openai.com/v1/responses/input_tokens");
        assert_eq!(t.provider, "openai");
        assert!(t.body.get("max_output_tokens").is_none());
        assert!(ProbeTarget::openai("bad\nkey", "http://x").is_err());
    }
}
