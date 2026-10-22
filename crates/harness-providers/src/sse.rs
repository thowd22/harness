//! Incremental Server-Sent Events parser.
//!
//! Follows the WHATWG event-stream rules: `data` fields on several lines are
//! joined with `\n`, lines starting with `:` are comments (keep-alives),
//! `\n`, `\r\n` and `\r` all end a line, and an empty line dispatches the
//! event. Bytes may arrive in arbitrary chunks, including splits inside a
//! line ending or a multi-byte UTF-8 character.

use std::collections::VecDeque;

use bytes::Bytes;
use futures::stream::{Stream, StreamExt};

use crate::provider::ProviderError;

/// One dispatched SSE event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// Event type; `message` when no `event:` field was sent.
    pub event: String,
    /// Data lines joined with `\n`.
    pub data: String,
    /// Last event id seen on the stream, if any.
    pub id: Option<String>,
    /// Reconnection time in milliseconds, if the event set one.
    pub retry: Option<u64>,
}

/// Incremental parser. Feed chunks with [`SseParser::push`].
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    skip_lf: bool,
    started: bool,
    event: Option<String>,
    data: String,
    has_data: bool,
    id: Option<String>,
    retry: Option<u64>,
}

impl SseParser {
    /// A new parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk and returns the events it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let mut out = Vec::new();
        let mut chunk = chunk;
        if self.skip_lf && !chunk.is_empty() {
            self.skip_lf = false;
            if chunk[0] == b'\n' {
                chunk = &chunk[1..];
            }
        }
        self.buf.extend_from_slice(chunk);
        let mut start = 0;
        let mut i = 0;
        while i < self.buf.len() {
            match self.buf[i] {
                b'\n' | b'\r' => {
                    let line = self.buf[start..i].to_vec();
                    let is_cr = self.buf[i] == b'\r';
                    i += 1;
                    if is_cr {
                        if i < self.buf.len() {
                            if self.buf[i] == b'\n' {
                                i += 1;
                            }
                        } else {
                            self.skip_lf = true;
                        }
                    }
                    start = i;
                    if let Some(ev) = self.line(&line) {
                        out.push(ev);
                    }
                }
                _ => i += 1,
            }
        }
        self.buf.drain(..start);
        out
    }

    /// Ends the stream. An unterminated final event is dispatched rather
    /// than dropped, to be lenient with servers that omit the last blank line.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            if let Some(ev) = self.line(&line) {
                return Some(ev);
            }
        }
        self.dispatch()
    }

    fn line(&mut self, raw: &[u8]) -> Option<SseEvent> {
        let mut raw = raw;
        if !self.started {
            self.started = true;
            raw = raw.strip_prefix("\u{feff}".as_bytes()).unwrap_or(raw);
        }
        if raw.is_empty() {
            return self.dispatch();
        }
        if raw[0] == b':' {
            return None;
        }
        let line = String::from_utf8_lossy(raw);
        let (field, value) = match line.find(':') {
            Some(pos) => {
                let v = &line[pos + 1..];
                (&line[..pos], v.strip_prefix(' ').unwrap_or(v))
            }
            None => (line.as_ref(), ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
            "retry" => {
                if let Ok(ms) = value.parse() {
                    self.retry = Some(ms);
                }
            }
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = self.event.take();
        let retry = self.retry.take();
        if !self.has_data {
            return None;
        }
        self.has_data = false;
        Some(SseEvent {
            event: event
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| "message".to_owned()),
            data: std::mem::take(&mut self.data),
            id: self.id.clone(),
            retry,
        })
    }
}

/// Adapts a byte stream into a stream of SSE events. Byte-stream errors are
/// passed through and end the stream.
pub fn sse_stream<S>(bytes: S) -> impl Stream<Item = Result<SseEvent, ProviderError>> + Send
where
    S: Stream<Item = Result<Bytes, ProviderError>> + Send + Unpin,
{
    struct St<S> {
        inner: S,
        parser: SseParser,
        pending: VecDeque<SseEvent>,
        ended: bool,
    }
    let st = St {
        inner: bytes,
        parser: SseParser::new(),
        pending: VecDeque::new(),
        ended: false,
    };
    futures::stream::unfold(st, |mut st| async move {
        loop {
            if let Some(ev) = st.pending.pop_front() {
                return Some((Ok(ev), st));
            }
            if st.ended {
                return None;
            }
            match st.inner.next().await {
                Some(Ok(chunk)) => st.pending.extend(st.parser.push(&chunk)),
                Some(Err(e)) => {
                    st.ended = true;
                    st.pending.clear();
                    return Some((Err(e), st));
                }
                None => {
                    st.ended = true;
                    st.pending.extend(st.parser.finish());
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        let mut out: Vec<_> = chunks.iter().flat_map(|c| p.push(c)).collect();
        out.extend(p.finish());
        out
    }

    fn ev(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
            id: None,
            retry: None,
        }
    }

    #[test]
    fn multi_line_data_is_joined() {
        let out = parse_all(&[b"event: delta\ndata: line1\ndata: line2\ndata:line3\n\n"]);
        assert_eq!(out, vec![ev("delta", "line1\nline2\nline3")]);
    }

    #[test]
    fn comments_and_keep_alives_are_ignored() {
        let out = parse_all(&[b": ping\n\n:\n\ndata: a\n: mid comment\n\n\n\n"]);
        assert_eq!(out, vec![ev("message", "a")]);
    }

    #[test]
    fn every_split_point_gives_the_same_events() {
        let input = "event: x\r\ndata: h\u{e9}llo \u{1f600}\r\nid: 7\r\nretry: 1500\r\n\r\ndata: b\rdata: c\r\r: k\n\ndata: {\"a\":1}\n\n";
        let bytes = input.as_bytes();
        let whole = parse_all(&[bytes]);
        assert_eq!(whole.len(), 3);
        assert_eq!(
            whole[0],
            SseEvent {
                event: "x".into(),
                data: "h\u{e9}llo \u{1f600}".into(),
                id: Some("7".into()),
                retry: Some(1500)
            }
        );
        assert_eq!(
            whole[1],
            SseEvent {
                id: Some("7".into()),
                ..ev("message", "b\nc")
            }
        );
        assert_eq!(whole[2].data, "{\"a\":1}");
        for split in 0..bytes.len() {
            assert_eq!(
                parse_all(&[&bytes[..split], &bytes[split..]]),
                whole,
                "split at {split}"
            );
        }
        let singles: Vec<&[u8]> = bytes.chunks(1).collect();
        assert_eq!(parse_all(&singles), whole);
    }

    #[test]
    fn crlf_split_across_chunks_is_one_line_ending() {
        let out = parse_all(&[b"data: a\r", b"\n", b"\r", b"\n"]);
        assert_eq!(out, vec![ev("message", "a")]);
    }

    #[test]
    fn event_without_data_is_not_dispatched_and_type_resets() {
        let out = parse_all(&[b"event: lonely\n\ndata: x\n\n"]);
        assert_eq!(out, vec![ev("message", "x")]);
    }

    #[test]
    fn unterminated_last_event_is_flushed_and_bom_skipped() {
        let out = parse_all(&["\u{feff}data: tail".as_bytes()]);
        assert_eq!(out, vec![ev("message", "tail")]);
        let out = parse_all(&[b"data\n\n"]);
        assert_eq!(out, vec![ev("message", "")]);
    }

    #[tokio::test]
    async fn stream_adapter_parses_and_propagates_errors() {
        let chunks: Vec<Result<Bytes, ProviderError>> = vec![
            Ok(Bytes::from_static(b"data: 1\n")),
            Ok(Bytes::from_static(b"\ndata: 2\n\n")),
            Err(ProviderError::Stalled(std::time::Duration::from_secs(1))),
            Ok(Bytes::from_static(b"data: never\n\n")),
        ];
        let out: Vec<_> = sse_stream(futures::stream::iter(chunks)).collect().await;
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].as_ref().unwrap().data, "1");
        assert_eq!(out[1].as_ref().unwrap().data, "2");
        assert!(matches!(out[2], Err(ProviderError::Stalled(_))));
    }
}
