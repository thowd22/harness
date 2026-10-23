//! Secret redaction for events and logs.
//!
//! A [`Redactor`] masks two kinds of secrets with [`MASK`]:
//!
//! - **Known secrets**: exact strings handed to [`Redactor::new`], typically
//!   every credential resolved from `harness.ini`. This crate does not read
//!   config itself; the caller collects the strings.
//! - **Built-in patterns** ([`BUILTIN_PATTERNS`]): API key prefixes (`sk-`,
//!   `ghp_`, `AKIA`, `xoxb-`, `AIza`, ...), JWTs, bearer/basic auth headers,
//!   `UPPER_CASE_SECRET=value` assignments, credentials in URLs and PEM private
//!   key blocks (also unterminated ones, which are masked to the end).
//!
//! Streaming text is redacted with a [`StreamRedactor`], which holds back just
//! enough of the tail that a secret split across chunks is still masked.
//!
//! ```
//! use std::sync::Arc;
//! use harness_events::redact::{Redactor, MASK};
//!
//! let r = Arc::new(Redactor::new(["hunter2-very-secret"]));
//! assert_eq!(r.redact("pw is hunter2-very-secret"), format!("pw is {MASK}"));
//! assert_eq!(r.redact("Authorization: Bearer abcdef0123456789xyz"),
//!            format!("Authorization: Bearer {MASK}"));
//!
//! let mut s = r.stream();
//! let mut out = s.push("key hunter2-ve");
//! out += &s.push("ry-secret done");
//! out += &s.finish();
//! assert_eq!(out, format!("key {MASK} done"));
//! ```

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use regex::Regex;

use crate::logging::LineRedactor;
use crate::schema::{EventKind, Payload};

/// Replacement text for every redacted secret.
pub const MASK: &str = "[REDACTED]";

/// Known secrets shorter than this are ignored, because masking very short
/// strings everywhere would destroy unrelated text.
pub const MIN_SECRET_LEN: usize = 4;

/// Minimum number of trailing bytes a [`StreamRedactor`] holds back, so a
/// pattern secret that has not matched yet (too short so far) is never
/// released early.
pub const PATTERN_HOLD: usize = 128;

/// Built-in credential patterns as `(name, regex)`. When a regex has a group
/// named `secret`, only that group is masked; otherwise the whole match is.
pub const BUILTIN_PATTERNS: &[(&str, &str)] = &[
    (
        "private_key_block",
        r"-----BEGIN[A-Z0-9 ]*PRIVATE KEY-----(?s:.*?)(?:-----END[A-Z0-9 ]*PRIVATE KEY-----|\z)",
    ),
    ("sk_api_key", r"\b(?:sk|rk)-[A-Za-z0-9][A-Za-z0-9_-]{19,}"),
    ("stripe_key", r"\b[sr]k_(?:live|test)_[A-Za-z0-9]{16,}"),
    (
        "github_token",
        r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,})",
    ),
    ("aws_access_key_id", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
    ("slack_token", r"\bxox[abposr]-[A-Za-z0-9-]{10,}"),
    ("google_api_key", r"\bAIza[0-9A-Za-z_-]{35}"),
    (
        "jwt",
        r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    ),
    (
        "auth_header",
        r"(?i)\b(?:bearer|basic)\s+(?P<secret>[A-Za-z0-9._~+/=-]{12,})",
    ),
    (
        "env_assignment",
        r#"\b[A-Z0-9_]*(?:API_?KEY|SECRET|PASSWORD|PASSWD|TOKEN|ACCESS_KEY|PRIVATE_KEY)[A-Z0-9_]*\s*[:=]\s*["']?(?P<secret>[^\s"']{6,})"#,
    ),
    (
        "url_credentials",
        r"\b[a-zA-Z][a-zA-Z0-9+.-]*://[^\s:/@]+:(?P<secret>[^\s/@]+)@",
    ),
];

/// One secret occurrence: what to mask, and the full regex match.
#[derive(Debug, Clone)]
struct Hit {
    mask: Range<usize>,
    full: Range<usize>,
}

/// Masks known secrets and built-in credential patterns.
///
/// Cheap to share: wrap in an [`Arc`] and clone the `Arc`.
pub struct Redactor {
    literals: Option<Regex>,
    max_literal: usize,
    patterns: Vec<Regex>,
}

impl fmt::Debug for Redactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the secrets themselves.
        f.debug_struct("Redactor")
            .field("known_secrets", &self.literals.is_some())
            .field("patterns", &self.patterns.len())
            .finish()
    }
}

impl Default for Redactor {
    /// Built-in patterns only.
    fn default() -> Self {
        Self::new(std::iter::empty::<&str>())
    }
}

impl Redactor {
    /// A redactor for the given known secrets plus [`BUILTIN_PATTERNS`].
    /// Secrets shorter than [`MIN_SECRET_LEN`] bytes are ignored.
    pub fn new<I, S>(secrets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut lits: Vec<String> = secrets
            .into_iter()
            .map(|s| s.as_ref().to_owned())
            .filter(|s| s.len() >= MIN_SECRET_LEN)
            .collect();
        // Longest first so a secret containing another is masked whole.
        lits.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        lits.dedup();
        let max_literal = lits.first().map_or(0, String::len);
        let literals = (!lits.is_empty()).then(|| {
            let alt: Vec<String> = lits.iter().map(|s| regex::escape(s)).collect();
            Regex::new(&alt.join("|")).expect("escaped literals form a valid regex")
        });
        let patterns = BUILTIN_PATTERNS
            .iter()
            .map(|(_, re)| Regex::new(re).expect("built-in pattern is valid"))
            .collect();
        Self {
            literals,
            max_literal,
            patterns,
        }
    }

    /// A [`StreamRedactor`] using this redactor's rules.
    pub fn stream(self: &Arc<Self>) -> StreamRedactor {
        StreamRedactor {
            redactor: Arc::clone(self),
            pending: String::new(),
        }
    }

    /// Bytes a [`StreamRedactor`] must hold back at minimum.
    pub fn hold_len(&self) -> usize {
        self.max_literal.max(PATTERN_HOLD)
    }

    /// All secret occurrences, sorted by start, overlapping masks merged.
    fn hits(&self, text: &str) -> Vec<Hit> {
        let mut hits = Vec::new();
        if let Some(lit) = &self.literals {
            hits.extend(lit.find_iter(text).map(|m| Hit {
                mask: m.range(),
                full: m.range(),
            }));
        }
        for re in &self.patterns {
            for caps in re.captures_iter(text) {
                let full = caps.get(0).expect("group 0 always matches").range();
                let mask = caps.name("secret").map_or(full.clone(), |m| m.range());
                hits.push(Hit { mask, full });
            }
        }
        hits.sort_by_key(|h| (h.mask.start, h.mask.end));
        let mut merged: Vec<Hit> = Vec::with_capacity(hits.len());
        for h in hits {
            match merged.last_mut() {
                Some(last) if h.mask.start <= last.mask.end => {
                    last.mask.end = last.mask.end.max(h.mask.end);
                    last.full.start = last.full.start.min(h.full.start);
                    last.full.end = last.full.end.max(h.full.end);
                }
                _ => merged.push(h),
            }
        }
        merged
    }

    /// Returns `text` with every secret replaced by [`MASK`].
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let hits = self.hits(text);
        if hits.is_empty() {
            Cow::Borrowed(text)
        } else {
            Cow::Owned(apply_masks(text, &hits))
        }
    }

    /// Redacts a string in place.
    pub fn redact_string(&self, s: &mut String) {
        if let Cow::Owned(r) = self.redact(s) {
            *s = r;
        }
    }

    /// Redacts every string value (not object keys) in a JSON value.
    pub fn redact_json(&self, value: &mut serde_json::Value) {
        use serde_json::Value;
        match value {
            Value::String(s) => self.redact_string(s),
            Value::Array(items) => items.iter_mut().for_each(|v| self.redact_json(v)),
            Value::Object(map) => map.values_mut().for_each(|v| self.redact_json(v)),
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    /// Redacts an inline payload. Blob references are left alone: blobs are
    /// written from already-redacted payloads.
    pub fn redact_payload(&self, payload: &mut Payload) {
        match payload {
            Payload::Text(s) => self.redact_string(s),
            Payload::Json(v) => self.redact_json(v),
            Payload::Blob(_) => {}
        }
    }

    /// Redacts every free-text field and payload of an event. Identifiers
    /// (call ids, node ids, enum tags) are left alone.
    pub fn redact_event(&self, kind: &mut EventKind) {
        for payload in kind.payloads_mut() {
            self.redact_payload(payload);
        }
        let r = |s: &mut String| self.redact_string(s);
        let ro = |s: &mut Option<String>| {
            if let Some(s) = s {
                self.redact_string(s);
            }
        };
        match kind {
            EventKind::RunStarted {
                name,
                workflow,
                host,
                title,
                agent_names,
            } => {
                r(name);
                ro(workflow);
                ro(host);
                ro(title);
                for v in agent_names.values_mut() {
                    r(v);
                }
            }
            EventKind::RunFinished { error, .. } | EventKind::NodeFinished { error, .. } => {
                ro(error)
            }
            EventKind::NodeStarted {
                name,
                agent,
                model,
                host,
                ..
            } => {
                r(name);
                ro(agent);
                ro(model);
                ro(host);
            }
            EventKind::NodeRetry { reason, .. } => r(reason),
            EventKind::ModelCallStarted { .. } | EventKind::ToolCall { .. } => {}
            EventKind::Delta { text, .. } => r(text),
            EventKind::ModelCallFinished { stop_reason, .. } => ro(stop_reason),
            EventKind::ApprovalRequested { action, .. } => r(action),
            EventKind::FileChanged { path, .. } => r(path),
            EventKind::VerifyStarted { command } | EventKind::VerifyFinished { command, .. } => {
                r(command)
            }
            EventKind::Memory { summary, .. } => r(summary),
            EventKind::Error {
                message, action, ..
            } => {
                r(message);
                if let Some(a) = action {
                    r(&mut a.label);
                    r(&mut a.url);
                }
            }
            EventKind::UserMessage { text } | EventKind::ToolOutput { text, .. } => r(text),
            EventKind::PolicyDecision {
                rule,
                reason,
                origin,
                ..
            } => {
                r(rule);
                r(reason);
                r(origin);
            }
            EventKind::RemoteOp { target, error, .. } => {
                r(target);
                ro(error);
            }
            EventKind::RemoteConnection { detail, .. } => ro(detail),
            EventKind::McpCall { server, tool, .. } => {
                r(server);
                r(tool);
            }
            EventKind::McpToolsChanged {
                server,
                added,
                removed,
                ..
            } => {
                r(server);
                for t in added.iter_mut().chain(removed.iter_mut()) {
                    r(t);
                }
            }
            EventKind::Tasks { goal, tasks, .. } => {
                ro(goal);
                for t in tasks {
                    r(&mut t.title);
                }
            }
            EventKind::NodeRunning
            | EventKind::ToolResult { .. }
            | EventKind::ApprovalResolved { .. }
            | EventKind::Cost { .. } => {}
        }
    }
}

impl LineRedactor for Redactor {
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        Redactor::redact(self, text)
    }
}

fn apply_masks(text: &str, hits: &[Hit]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for h in hits {
        out.push_str(&text[pos..h.mask.start]);
        out.push_str(MASK);
        pos = h.mask.end;
    }
    out.push_str(&text[pos..]);
    out
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Redacts a text stream delivered in chunks.
///
/// [`push`](Self::push) returns the redacted text that is safe to release;
/// the last [`Redactor::hold_len`] bytes, and any secret match that reaches
/// the end of the buffer, are held until more text arrives or
/// [`finish`](Self::finish) is called. Concatenating every returned piece
/// equals redacting the whole stream at once.
pub struct StreamRedactor {
    redactor: Arc<Redactor>,
    pending: String,
}

impl fmt::Debug for StreamRedactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamRedactor")
            .field("pending_len", &self.pending.len())
            .finish()
    }
}

impl StreamRedactor {
    /// Adds a chunk and returns the redacted text that can be released now
    /// (possibly empty).
    pub fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let hits = self.redactor.hits(&self.pending);
        let hold = self.redactor.hold_len();
        let mut cut = floor_char_boundary(&self.pending, self.pending.len().saturating_sub(hold));
        // Never cut through a match; pull the cut back to its start. Matches
        // are sorted by start, so one backwards pass reaches a fixed point.
        for h in hits.iter().rev() {
            if h.full.end > cut && h.full.start.min(h.mask.start) < cut {
                cut = h.full.start.min(h.mask.start);
            }
        }
        let released: Vec<Hit> = hits.into_iter().filter(|h| h.mask.end <= cut).collect();
        let out = apply_masks(&self.pending[..cut], &released);
        self.pending.drain(..cut);
        out
    }

    /// Releases everything still held, redacted.
    pub fn finish(&mut self) -> String {
        let rest = std::mem::take(&mut self.pending);
        self.redactor.redact(&rest).into_owned()
    }

    /// Bytes currently held back.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{BlobEncoding, BlobRef};

    const ANT: &str = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";

    fn r() -> Arc<Redactor> {
        Arc::new(Redactor::new(["ini-secret-value-1", "short-ish", "abc"]))
    }

    #[test]
    fn masks_known_secrets_from_config() {
        let r = r();
        assert_eq!(
            r.redact("a ini-secret-value-1 b short-ish c"),
            format!("a {MASK} b {MASK} c")
        );
        // Too-short secrets are ignored.
        assert_eq!(r.redact("abc"), "abc");
        assert!(matches!(r.redact("nothing here"), Cow::Borrowed(_)));
    }

    #[test]
    fn masks_builtin_patterns() {
        let r = Redactor::default();
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----";
        let cases: Vec<(String, String)> = vec![
            (format!("key={ANT}!"), format!("key={MASK}!")),
            (
                "export OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwx".into(),
                format!("export OPENAI_API_KEY={MASK}"),
            ),
            (
                "ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
                MASK.into(),
            ),
            ("id AKIAIOSFODNN7EXAMPLE ok".into(), format!("id {MASK} ok")),
            ("xoxb-1234567890-abcdef".into(), MASK.into()),
            (format!("AIza{}", "x".repeat(35)), MASK.into()),
            (
                "Authorization: Bearer abc.def-ghi_jkl123".into(),
                format!("Authorization: Bearer {MASK}"),
            ),
            (
                "DB_PASSWORD='p4ssw0rd!x'".into(),
                format!("DB_PASSWORD='{MASK}'"),
            ),
            (
                "https://user:hunter22@example.com/x".into(),
                format!("https://user:{MASK}@example.com/x"),
            ),
            (
                format!("before\n{pem}\nafter"),
                format!("before\n{MASK}\nafter"),
            ),
            (
                "-----BEGIN RSA PRIVATE KEY-----\nMIIEow".into(),
                MASK.into(),
            ),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N".into(),
                MASK.into(),
            ),
        ];
        for (input, want) in cases {
            assert_eq!(r.redact(&input), want, "input: {input}");
        }
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        let r = Redactor::default();
        for s in [
            "let password = get_password();",
            "tokens: 12345678 in, 400 out",
            "const API_KEY: &str = env();",
            "see https://example.com/path@v2",
            "risk-assessment and desk-top",
        ] {
            assert_eq!(r.redact(s), s);
        }
    }

    #[test]
    fn secrets_split_across_stream_chunks_are_masked() {
        let r = r();
        let text = format!(
            "env: ANTHROPIC_KEY {ANT} then ini-secret-value-1 and Bearer abcdefghijklmnop0123 \
             -----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY----- end"
        );
        let expected = r.redact(&text).into_owned();
        assert!(!expected.contains("ini-secret"));
        // Every chunk size, including 1 byte, must give the same result.
        for size in [1, 2, 3, 5, 7, 16, 33, 200] {
            let mut s = r.stream();
            let mut out = String::new();
            let chars: Vec<char> = text.chars().collect();
            for chunk in chars.chunks(size) {
                let chunk: String = chunk.iter().collect();
                let piece = s.push(&chunk);
                assert!(
                    !piece.contains("ini-secret-va"),
                    "leaked at size {size}: {piece}"
                );
                out.push_str(&piece);
            }
            out.push_str(&s.finish());
            assert_eq!(out, expected, "chunk size {size}");
            assert!(!out.contains(&ANT[10..]), "chunk size {size}");
        }
    }

    #[test]
    fn stream_releases_text_without_waiting_for_finish() {
        let r = r();
        let mut s = r.stream();
        let long = "x".repeat(1000);
        let out = s.push(&long);
        assert_eq!(out.len(), 1000 - r.hold_len());
        assert_eq!(s.pending_len(), r.hold_len());
        assert_eq!(s.finish().len(), r.hold_len());
    }

    #[test]
    fn stream_handles_multibyte_text() {
        let r = r();
        let mut s = r.stream();
        let text = "é".repeat(300);
        let mut out = s.push(&text);
        out += &s.finish();
        assert_eq!(out, text);
    }

    #[test]
    fn redacts_event_fields_and_payloads() {
        let r = r();
        let mut kind = EventKind::ToolResult {
            call_id: "c1".into(),
            ok: true,
            output: Payload::Json(serde_json::json!({"stdout": ["x ini-secret-value-1"]})),
            duration_ms: 1,
        };
        r.redact_event(&mut kind);
        let json = serde_json::to_string(&kind).unwrap();
        assert!(!json.contains("ini-secret"), "{json}");
        assert!(json.contains(MASK));

        // Run titles come from user inputs, so they are redacted too.
        let mut kind = EventKind::RunStarted {
            name: "fix".into(),
            workflow: Some("fix".into()),
            host: None,
            title: Some("deploy with ini-secret-value-1".into()),
            agent_names: [("coder".to_owned(), "ini-secret-value-1".to_owned())].into(),
        };
        r.redact_event(&mut kind);
        let json = serde_json::to_string(&kind).unwrap();
        assert!(!json.contains("ini-secret"), "{json}");

        let mut kind = EventKind::Error {
            message: format!("bad {ANT}"),
            recoverable: false,
            action: Some(crate::schema::ErrorAction {
                label: "Manage usage".into(),
                url: format!("https://example.com/?k={ANT}"),
            }),
        };
        r.redact_event(&mut kind);
        assert_eq!(
            kind,
            EventKind::Error {
                message: format!("bad {MASK}"),
                recoverable: false,
                action: Some(crate::schema::ErrorAction {
                    label: "Manage usage".into(),
                    url: format!("https://example.com/?k={MASK}"),
                }),
            }
        );

        let blob = Payload::Blob(BlobRef {
            sha256: "ab".into(),
            size: 1,
            encoding: BlobEncoding::Text,
        });
        let mut p = blob.clone();
        r.redact_payload(&mut p);
        assert_eq!(p, blob);
    }

    #[test]
    fn debug_does_not_print_secrets() {
        let dbg = format!("{:?}", r());
        assert!(!dbg.contains("ini-secret"), "{dbg}");
    }

    #[test]
    fn works_as_log_line_redactor() {
        let lr: Arc<dyn LineRedactor> = r();
        assert_eq!(lr.redact("x ini-secret-value-1"), format!("x {MASK}"));
    }
}
