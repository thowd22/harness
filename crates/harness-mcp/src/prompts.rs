//! MCP prompts and resources (`prompts/list`, `prompts/get`,
//! `resources/list`, `resources/read`).
//!
//! Prompts are user-controlled templates: the TUI offers them as slash
//! commands namespaced by server (`/github:review-pr`). Resources are
//! application-driven context: the TUI lets the user attach one to the next
//! message, labeled as untrusted content. Everything a server sends here is
//! untrusted text; callers redact it and cap its size before showing it or
//! sending it to a model.

use serde_json::Value;

/// The notification a server sends when its prompt list changed.
pub const PROMPTS_LIST_CHANGED: &str = "notifications/prompts/list_changed";
/// The notification a server sends when its resource list changed.
pub const RESOURCES_LIST_CHANGED: &str = "notifications/resources/list_changed";

/// Most items read from one paginated list.
pub const MAX_LIST_ITEMS: usize = 1000;

/// True when `capabilities` declares `prompts`.
pub fn has_prompts(capabilities: &Value) -> bool {
    capabilities.get("prompts").is_some_and(Value::is_object)
}

/// True when `capabilities` declares `resources`.
pub fn has_resources(capabilities: &Value) -> bool {
    capabilities.get("resources").is_some_and(Value::is_object)
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// One argument of a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptArgument {
    /// Argument name.
    pub name: String,
    /// Description (untrusted server text).
    pub description: Option<String>,
    /// Whether the server requires it.
    pub required: bool,
}

/// A prompt template offered by a server (`prompts/list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// Unique prompt name.
    pub name: String,
    /// Display title.
    pub title: Option<String>,
    /// Description (untrusted server text).
    pub description: Option<String>,
    /// Arguments, in the server's order.
    pub arguments: Vec<PromptArgument>,
}

impl Prompt {
    /// Parses a prompt definition; `None` without a name.
    pub fn from_value(v: &Value) -> Option<Self> {
        let name = opt_str(v, "name")?;
        let arguments = v
            .get("arguments")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|arg| {
                        Some(PromptArgument {
                            name: opt_str(arg, "name")?,
                            description: opt_str(arg, "description"),
                            required: arg
                                .get("required")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            name,
            title: opt_str(v, "title"),
            description: opt_str(v, "description"),
            arguments,
        })
    }
}

/// One message of a `prompts/get` result.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptMessage {
    /// `user` or `assistant`.
    pub role: String,
    /// The content block (`text`, `image`, `audio`, `resource_link`,
    /// `resource`).
    pub content: Value,
}

/// The result of `prompts/get`.
#[derive(Debug, Clone, PartialEq)]
pub struct GetPromptResult {
    /// Description (untrusted server text).
    pub description: Option<String>,
    /// The messages.
    pub messages: Vec<PromptMessage>,
}

/// How a content block of a prompt message reads as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptPart {
    /// Plain text.
    Text(String),
    /// An embedded resource with text content: label it as untrusted.
    Resource {
        /// Its URI.
        uri: String,
        /// Its text.
        text: String,
    },
    /// Something that is not sent as text (image, audio, binary resource,
    /// resource link): a short note.
    Note(String),
}

impl GetPromptResult {
    /// Parses a `GetPromptResult`.
    pub fn from_value(v: &Value) -> Self {
        Self {
            description: opt_str(v, "description"),
            messages: v
                .get("messages")
                .and_then(Value::as_array)
                .map(|m| {
                    m.iter()
                        .map(|msg| PromptMessage {
                            role: msg
                                .get("role")
                                .and_then(Value::as_str)
                                .unwrap_or("user")
                                .to_owned(),
                            content: msg.get("content").cloned().unwrap_or(Value::Null),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Each message as `(role, part)`, in order.
    pub fn parts(&self) -> Vec<(String, PromptPart)> {
        self.messages
            .iter()
            .map(|m| (m.role.clone(), content_part(&m.content)))
            .collect()
    }
}

/// Reads one content block.
fn content_part(c: &Value) -> PromptPart {
    let mime = |v: &Value| opt_str(v, "mimeType").unwrap_or_else(|| "unknown type".into());
    match c.get("type").and_then(Value::as_str).unwrap_or_default() {
        "text" => PromptPart::Text(
            c.get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
        "resource" => {
            let r = c.get("resource").cloned().unwrap_or(Value::Null);
            let uri = opt_str(&r, "uri").unwrap_or_default();
            match r.get("text").and_then(Value::as_str) {
                Some(t) => PromptPart::Resource {
                    uri,
                    text: t.to_owned(),
                },
                None => PromptPart::Note(format!(
                    "[binary resource {uri} ({}) not included]",
                    mime(&r)
                )),
            }
        }
        "resource_link" => PromptPart::Note(format!(
            "[resource link: {}]",
            opt_str(c, "uri").unwrap_or_default()
        )),
        "image" => PromptPart::Note(format!("[image ({}) not included]", mime(c))),
        "audio" => PromptPart::Note(format!("[audio ({}) not included]", mime(c))),
        other => PromptPart::Note(format!("[{other} content not included]")),
    }
}

/// A resource offered by a server (`resources/list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    /// Its URI.
    pub uri: String,
    /// Its name.
    pub name: String,
    /// Display title.
    pub title: Option<String>,
    /// Description (untrusted server text).
    pub description: Option<String>,
    /// MIME type.
    pub mime_type: Option<String>,
    /// Size in bytes.
    pub size: Option<u64>,
}

impl Resource {
    /// Parses a resource; `None` without a URI. The name defaults to the
    /// URI.
    pub fn from_value(v: &Value) -> Option<Self> {
        let uri = opt_str(v, "uri")?;
        Some(Self {
            name: opt_str(v, "name").unwrap_or_else(|| uri.clone()),
            title: opt_str(v, "title"),
            description: opt_str(v, "description"),
            mime_type: opt_str(v, "mimeType"),
            size: v.get("size").and_then(Value::as_u64),
            uri,
        })
    }
}

/// One item of a `resources/read` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceContents {
    /// Its URI.
    pub uri: String,
    /// MIME type.
    pub mime_type: Option<String>,
    /// Text content, if it is text.
    pub text: Option<String>,
    /// Length of the base64 `blob`, if it is binary.
    pub blob_len: Option<usize>,
}

/// The result of `resources/read`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadResourceResult {
    /// The contents (several for a directory-like resource).
    pub contents: Vec<ResourceContents>,
}

impl ReadResourceResult {
    /// Parses a `ReadResourceResult`.
    pub fn from_value(v: &Value) -> Self {
        Self {
            contents: v
                .get("contents")
                .and_then(Value::as_array)
                .map(|c| {
                    c.iter()
                        .map(|item| ResourceContents {
                            uri: opt_str(item, "uri").unwrap_or_default(),
                            mime_type: opt_str(item, "mimeType"),
                            text: item.get("text").and_then(Value::as_str).map(str::to_owned),
                            blob_len: item.get("blob").and_then(Value::as_str).map(str::len),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The text of every item; binary items become a short note. Several
    /// items are each headed by their URI.
    pub fn text(&self) -> String {
        let several = self.contents.len() > 1;
        self.contents
            .iter()
            .map(|c| {
                let body = match (&c.text, c.blob_len) {
                    (Some(t), _) => t.clone(),
                    (None, Some(n)) => format!(
                        "[binary content ({}, about {} bytes) not included]",
                        c.mime_type.as_deref().unwrap_or("unknown type"),
                        n / 4 * 3
                    ),
                    (None, None) => "[empty]".to_owned(),
                };
                if several {
                    format!("--- {}\n{body}", c.uri)
                } else {
                    body
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prompts_parse_with_arguments() {
        let p = Prompt::from_value(&json!({
            "name": "review-pr", "title": "Review a PR", "description": "Reviews",
            "arguments": [{"name": "pr", "required": true}, {"name": "focus", "description": "what"}, {"nope": 1}]
        }))
        .unwrap();
        assert_eq!(p.name, "review-pr");
        assert_eq!(p.title.as_deref(), Some("Review a PR"));
        assert_eq!(p.arguments.len(), 2);
        assert!(p.arguments[0].required);
        assert!(!p.arguments[1].required);
        assert_eq!(p.arguments[1].description.as_deref(), Some("what"));
        assert!(Prompt::from_value(&json!({"title": "x"})).is_none());
    }

    #[test]
    fn prompt_results_read_as_parts() {
        let r = GetPromptResult::from_value(&json!({
            "description": "d",
            "messages": [
                {"role": "user", "content": {"type": "text", "text": "hello"}},
                {"role": "user", "content": {"type": "resource", "resource": {"uri": "a://b", "mimeType": "text/plain", "text": "body"}}},
                {"role": "assistant", "content": {"type": "image", "data": "AAAA", "mimeType": "image/png"}},
                {"role": "user", "content": {"type": "resource_link", "uri": "a://c", "name": "c"}},
                {"role": "user", "content": {"type": "resource", "resource": {"uri": "a://d", "blob": "AAAA"}}}
            ]
        }));
        let parts = r.parts();
        assert_eq!(parts[0], ("user".into(), PromptPart::Text("hello".into())));
        assert_eq!(
            parts[1].1,
            PromptPart::Resource {
                uri: "a://b".into(),
                text: "body".into()
            }
        );
        assert_eq!(parts[2].0, "assistant");
        assert_eq!(
            parts[2].1,
            PromptPart::Note("[image (image/png) not included]".into())
        );
        assert_eq!(
            parts[3].1,
            PromptPart::Note("[resource link: a://c]".into())
        );
        assert!(matches!(&parts[4].1, PromptPart::Note(n) if n.contains("binary resource a://d")));
    }

    #[test]
    fn resources_parse_and_read_as_text() {
        let r =
            Resource::from_value(&json!({"uri": "file:///x", "mimeType": "text/plain", "size": 3}))
                .unwrap();
        assert_eq!(r.name, "file:///x");
        assert_eq!(r.size, Some(3));
        assert!(Resource::from_value(&json!({"name": "x"})).is_none());
        let one =
            ReadResourceResult::from_value(&json!({"contents": [{"uri": "a", "text": "hi"}]}));
        assert_eq!(one.text(), "hi");
        let two = ReadResourceResult::from_value(&json!({"contents": [
            {"uri": "a", "text": "hi"}, {"uri": "b", "mimeType": "image/png", "blob": "AAAAAAAA"}
        ]}));
        assert_eq!(
            two.text(),
            "--- a\nhi\n--- b\n[binary content (image/png, about 6 bytes) not included]"
        );
        assert!(has_prompts(&json!({"prompts": {}})));
        assert!(!has_prompts(&json!({"tools": {}})));
        assert!(has_resources(&json!({"resources": {"subscribe": true}})));
    }
}
