//! Shared plumbing for the OpenAI-style providers: SSE events go through a
//! per-attempt [`EventMapper`] and come out as normalized [`StreamEvent`]s,
//! with retries handled by [`retry_stream`].

use std::collections::VecDeque;

use futures::{Stream, StreamExt};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::http::HttpClient;
use crate::provider::{EventStream, ProviderError, StreamEvent};
use crate::retry::{Attempt, retry_stream};
use crate::sse::{SseEvent, sse_stream};

/// Turns one attempt's SSE events into normalized events.
pub(crate) trait EventMapper: Send + 'static {
    /// Maps one SSE event to zero or more stream events.
    fn on_event(&mut self, event: &SseEvent) -> Result<Vec<StreamEvent>, ProviderError>;
    /// Called when the byte stream ends before [`EventMapper::finished`].
    fn on_end(&mut self) -> Result<Vec<StreamEvent>, ProviderError>;
    /// True once the mapper has emitted `MessageStop`; the stream then ends.
    fn finished(&self) -> bool;
}

/// Applies `mapper` to an SSE stream.
pub(crate) fn map_sse<S, M>(
    sse: S,
    mapper: M,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send
where
    S: Stream<Item = Result<SseEvent, ProviderError>> + Send + 'static,
    M: EventMapper,
{
    struct St<M> {
        sse: futures::stream::BoxStream<'static, Result<SseEvent, ProviderError>>,
        mapper: M,
        queue: VecDeque<StreamEvent>,
        done: bool,
    }
    let st = St {
        sse: sse.boxed(),
        mapper,
        queue: VecDeque::new(),
        done: false,
    };
    futures::stream::unfold(st, |mut st| async move {
        loop {
            if let Some(e) = st.queue.pop_front() {
                return Some((Ok(e), st));
            }
            if st.done {
                return None;
            }
            let result = match st.sse.next().await {
                Some(Ok(ev)) => st.mapper.on_event(&ev),
                Some(Err(e)) => Err(e),
                None => {
                    st.done = true;
                    st.mapper.on_end()
                }
            };
            match result {
                Ok(events) => {
                    st.queue.extend(events);
                    if st.mapper.finished() {
                        st.done = true;
                    }
                }
                Err(e) => {
                    st.done = true;
                    return Some((Err(e), st));
                }
            }
        }
    })
}

/// Opens a retried, cancellable SSE call and maps it with a fresh mapper
/// per attempt.
pub(crate) fn open_stream<M, F>(
    client: HttpClient,
    path: &'static str,
    headers: HeaderMap,
    body: Value,
    cancel: CancellationToken,
    new_mapper: F,
) -> EventStream
where
    M: EventMapper,
    F: Fn() -> M + Send + Sync + 'static,
{
    let policy = client.config().retry;
    let stream = retry_stream(policy, cancel, move || {
        let (client, headers, body) = (client.clone(), headers.clone(), body.clone());
        let mapper = new_mapper();
        async move {
            let bytes = client.open(path, &headers, &body).await?;
            Ok(map_sse(sse_stream(bytes), mapper))
        }
    });
    Box::pin(stream.map(|r| {
        r.map(|a| match a {
            Attempt::Item(e) => e,
            Attempt::Retry(r) => StreamEvent::Retry(r),
        })
    }))
}

/// A sensitive `Authorization: Bearer <key>` header map.
pub(crate) fn bearer_headers(api_key: &str) -> Result<HeaderMap, ProviderError> {
    let mut value = HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|_| ProviderError::Auth("API key contains invalid header characters".into()))?;
    value.set_sensitive(true);
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, value);
    Ok(headers)
}

/// Parses an SSE `data` payload as JSON.
pub(crate) fn parse_json(event: &SseEvent) -> Result<Value, ProviderError> {
    serde_json::from_str(&event.data).map_err(|e| {
        ProviderError::Decode(format!(
            "invalid JSON in `{}` event: {e}: {}",
            event.event,
            event.data.chars().take(200).collect::<String>()
        ))
    })
}

/// A `u64` at `pointer`, or 0.
pub(crate) fn u64_at(v: &Value, pointer: &str) -> u64 {
    v.pointer(pointer).and_then(Value::as_u64).unwrap_or(0)
}

/// Maps an in-stream error object with a string `code` (OpenAI style) or a
/// numeric HTTP-like `code` (OpenRouter style) to a typed error, so that
/// transient failures are retried.
pub(crate) fn stream_error(code: Option<&Value>, message: &str) -> ProviderError {
    let message = message.to_owned();
    match code {
        Some(Value::Number(n)) => match n.as_u64().and_then(|n| u16::try_from(n).ok()) {
            Some(status) => ProviderError::from_status(status, message, None),
            None => ProviderError::Other(message),
        },
        Some(Value::String(code))
            if crate::chatgpt::usage_limit_error(code, &message).is_some() =>
        {
            crate::chatgpt::usage_limit_error(code, &message).expect("checked")
        }
        Some(Value::String(code)) => match code.as_str() {
            "rate_limit_exceeded" | "rate_limit" => ProviderError::RateLimited {
                message,
                retry_after: None,
            },
            "server_is_overloaded" | "overloaded" | "slow_down" => ProviderError::Overloaded {
                message,
                retry_after: None,
            },
            "server_error" | "internal_error" | "api_error" => ProviderError::Http {
                status: 500,
                message,
                retry_after: None,
            },
            "invalid_prompt" | "invalid_request_error" | "context_length_exceeded" => {
                ProviderError::InvalidRequest(format!("{code}: {message}"))
            }
            _ if message.to_ascii_lowercase().contains("overloaded") => ProviderError::Overloaded {
                message,
                retry_after: None,
            },
            _ => ProviderError::Other(format!("{code}: {message}")),
        },
        _ => ProviderError::Other(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{StopKind, StopReason, Usage};
    use serde_json::json;

    struct Echo {
        done: bool,
    }

    impl EventMapper for Echo {
        fn on_event(&mut self, e: &SseEvent) -> Result<Vec<StreamEvent>, ProviderError> {
            if e.data == "stop" {
                self.done = true;
                return Ok(vec![StreamEvent::MessageStop {
                    stop: StopReason::new(StopKind::EndTurn),
                    usage: Usage::default(),
                }]);
            }
            Ok(vec![
                StreamEvent::TextDelta {
                    index: 0,
                    text: e.data.clone(),
                },
                StreamEvent::BlockStop { index: 0 },
            ])
        }
        fn on_end(&mut self) -> Result<Vec<StreamEvent>, ProviderError> {
            Err(ProviderError::Connection("ended early".into()))
        }
        fn finished(&self) -> bool {
            self.done
        }
    }

    fn ev(data: &str) -> Result<SseEvent, ProviderError> {
        Ok(SseEvent {
            event: "message".into(),
            data: data.into(),
            id: None,
            retry: None,
        })
    }

    #[tokio::test]
    async fn mapper_stops_after_finish_and_ignores_trailing_events() {
        let s = futures::stream::iter(vec![ev("a"), ev("stop"), ev("ignored")]);
        let out: Vec<_> = map_sse(s, Echo { done: false }).collect().await;
        assert_eq!(out.len(), 3);
        assert!(matches!(out[2], Ok(StreamEvent::MessageStop { .. })));
    }

    #[tokio::test]
    async fn early_end_reports_the_mapper_error() {
        let s = futures::stream::iter(vec![ev("a")]);
        let out: Vec<_> = map_sse(s, Echo { done: false }).collect().await;
        assert_eq!(
            out.last().unwrap(),
            &Err(ProviderError::Connection("ended early".into()))
        );
    }

    #[test]
    fn stream_errors_map_to_retryable_kinds() {
        assert!(matches!(
            stream_error(Some(&json!("rate_limit_exceeded")), "slow"),
            ProviderError::RateLimited { .. }
        ));
        assert!(
            stream_error(Some(&json!("server_error")), "x")
                .retry_reason()
                .is_some()
        );
        assert!(
            stream_error(Some(&json!(502)), "x")
                .retry_reason()
                .is_some()
        );
        assert!(matches!(
            stream_error(Some(&json!(401)), "x"),
            ProviderError::Auth(_)
        ));
        assert!(
            stream_error(Some(&json!("invalid_prompt")), "x")
                .retry_reason()
                .is_none()
        );
    }

    #[test]
    fn usage_limit_stream_errors_are_typed() {
        let e = stream_error(
            Some(&json!("subscription_sharing_usage_limit_exceeded")),
            "limit",
        );
        assert!(matches!(e, ProviderError::UsageLimit { .. }), "{e:?}");
        assert!(e.retry_reason().is_none());
    }

    #[test]
    fn bearer_header_is_sensitive() {
        let h = bearer_headers("sk-1").unwrap();
        assert!(h[AUTHORIZATION].is_sensitive());
        assert_eq!(h[AUTHORIZATION], "Bearer sk-1");
    }
}
