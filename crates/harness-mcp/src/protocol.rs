//! MCP wire types: protocol versions, `_meta` keys, error codes and the
//! JSON-RPC 2.0 messages the client sends and receives.
//!
//! harness is a *dual-era* client (MCP 2026-07-28 "Versioning and
//! Compatibility"): it prefers the modern, stateless protocol, where every
//! request carries its protocol version and client capabilities in `_meta`,
//! and falls back to the legacy `initialize` handshake for servers that
//! implement 2025-11-25 or earlier.

use serde_json::{Map, Value, json};

/// The modern protocol version harness speaks (per-request `_meta`).
pub const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";

/// Modern versions harness supports, preferred first.
pub const MODERN_VERSIONS: &[&str] = &[MODERN_PROTOCOL_VERSION];

/// Legacy (handshake-based) versions harness supports, preferred first.
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

/// `_meta` key carrying the request's protocol version.
pub const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
/// `_meta` key carrying the client name and version.
pub const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
/// `_meta` key carrying the client capabilities.
pub const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
/// `_meta` key a server uses to identify itself in results.
pub const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// JSON-RPC parse error.
pub const PARSE_ERROR: i64 = -32700;
/// JSON-RPC invalid request.
pub const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC method not found.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC invalid params.
pub const INVALID_PARAMS: i64 = -32602;
/// JSON-RPC internal error.
pub const INTERNAL_ERROR: i64 = -32603;
/// MCP: HTTP headers do not match the body.
pub const HEADER_MISMATCH: i64 = -32020;
/// MCP: a client capability the request needs was not declared.
pub const MISSING_REQUIRED_CLIENT_CAPABILITY: i64 = -32021;
/// MCP: the requested protocol version is not supported.
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// True for the error codes that identify a modern server.
pub fn is_modern_error(code: i64) -> bool {
    matches!(
        code,
        HEADER_MISMATCH | MISSING_REQUIRED_CLIENT_CAPABILITY | UNSUPPORTED_PROTOCOL_VERSION
    )
}

/// Which protocol generation a connection speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// Stateless, per-request `_meta` (2026-07-28 and later).
    Modern,
    /// `initialize` handshake (2025-11-25 and earlier).
    Legacy,
}

impl Era {
    /// Lower-case name for logs and reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Era::Modern => "modern",
            Era::Legacy => "legacy",
        }
    }
}

/// A JSON-RPC error object received from a server.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    /// Error code.
    pub code: i64,
    /// Human-readable message (from the server; untrusted).
    pub message: String,
    /// Optional extra data.
    pub data: Option<Value>,
}

impl RpcError {
    /// Parses an `error` member.
    pub fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            code: v.get("code")?.as_i64()?,
            message: v
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            data: v.get("data").cloned(),
        })
    }

    /// For [`UNSUPPORTED_PROTOCOL_VERSION`]: the versions the server lists.
    pub fn supported_versions(&self) -> Vec<String> {
        self.data
            .as_ref()
            .and_then(|d| d.get("supported"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

/// A message read from a server.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A response to one of our requests.
    Response {
        /// The request id.
        id: Value,
        /// `Ok(result)` or `Err(error)`; `Err` with code
        /// [`INVALID_REQUEST`] when the response has neither.
        outcome: Result<Value, RpcError>,
    },
    /// A notification (`notifications/progress`, `notifications/message`).
    Notification {
        /// Method name.
        method: String,
        /// Parameters.
        params: Value,
    },
    /// A server-to-client request (legacy servers only).
    Request {
        /// Request id.
        id: Value,
        /// Method name.
        method: String,
    },
}

/// Classifies a JSON value as a JSON-RPC message, or explains why it is not.
pub fn parse_incoming(v: &Value) -> Result<Incoming, String> {
    let obj = v.as_object().ok_or("not a JSON object")?;
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("missing `\"jsonrpc\": \"2.0\"`".into());
    }
    let id = obj.get("id").filter(|id| id.is_string() || id.is_number());
    match (id, obj.get("method").and_then(Value::as_str)) {
        (Some(id), Some(method)) => Ok(Incoming::Request {
            id: id.clone(),
            method: method.to_owned(),
        }),
        (None, Some(method)) => Ok(Incoming::Notification {
            method: method.to_owned(),
            params: obj.get("params").cloned().unwrap_or(Value::Null),
        }),
        (Some(id), None) => {
            let outcome = if let Some(r) = obj.get("result") {
                Ok(r.clone())
            } else if let Some(e) = obj.get("error").and_then(RpcError::from_value) {
                Err(e)
            } else {
                Err(RpcError {
                    code: INVALID_REQUEST,
                    message: "response has neither `result` nor a valid `error`".into(),
                    data: None,
                })
            };
            Ok(Incoming::Response {
                id: id.clone(),
                outcome,
            })
        }
        (None, None) => {
            if let Some(e) = obj.get("error").and_then(RpcError::from_value) {
                // An error without an id: the server could not read our
                // request at all.
                Ok(Incoming::Response {
                    id: Value::Null,
                    outcome: Err(e),
                })
            } else {
                Err("neither a request, a notification nor a response".into())
            }
        }
    }
}

/// The notification a server sends when its tool list changed.
pub const TOOLS_LIST_CHANGED: &str = "notifications/tools/list_changed";

/// `_meta` key carrying the subscription id on a `subscriptions/listen`
/// stream (modern servers).
pub const META_SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";

/// A notification from a server, as delivered to subscribers of a
/// transport.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    /// Method name.
    pub method: String,
    /// Parameters.
    pub params: Value,
}

/// How many notifications a transport buffers per subscriber.
pub const NOTIFICATION_BUFFER: usize = 64;

/// The `subscriptions/listen` parameters harness sends (modern servers
/// that declare `tools.listChanged`).
pub fn listen_params() -> Value {
    json!({"notifications": {"toolsListChanged": true}})
}

/// True when `capabilities` declares `tools.listChanged`.
pub fn tools_list_changed(capabilities: &Value) -> bool {
    capabilities
        .get("tools")
        .and_then(|t| t.get("listChanged"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Builds a JSON-RPC request.
pub fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// Builds a JSON-RPC notification.
pub fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

/// The client's `Implementation` object.
pub fn client_info() -> Value {
    json!({"name": "harness", "version": env!("CARGO_PKG_VERSION")})
}

/// The client's capabilities. harness offers no client features (roots,
/// sampling, elicitation) yet.
pub fn client_capabilities() -> Value {
    json!({})
}

/// Adds the modern per-request `_meta` fields to `params`.
pub fn with_modern_meta(params: Value, version: &str) -> Value {
    let mut obj = match params {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let meta = obj
        .entry("_meta")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(m) = meta {
        m.insert(META_PROTOCOL_VERSION.into(), json!(version));
        m.insert(META_CLIENT_INFO.into(), client_info());
        m.insert(META_CLIENT_CAPABILITIES.into(), client_capabilities());
    }
    Value::Object(obj)
}

/// Checks a result's `resultType`: absent or `complete` is a final result.
pub fn check_result_type(result: &Value) -> Result<(), String> {
    match result.get("resultType").and_then(Value::as_str) {
        None | Some("complete") => Ok(()),
        Some("input_required") => Err(
            "the server asked for more input (elicitation, sampling or roots), which harness does not support yet"
                .into(),
        ),
        Some(other) => Err(format!("unknown resultType `{other}`")),
    }
}

/// Self-reported server identity (display and logs only; never trusted).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerInfo {
    /// Server name.
    pub name: String,
    /// Server version.
    pub version: String,
}

impl ServerInfo {
    /// Parses an `Implementation` object.
    pub fn from_value(v: Option<&Value>) -> Self {
        let get = |k: &str| {
            v.and_then(|v| v.get(k))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        Self {
            name: get("name"),
            version: get("version"),
        }
    }
}

/// A tool offered by a server (`tools/list`).
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    /// Unique tool name.
    pub name: String,
    /// Description (untrusted text from the server).
    pub description: Option<String>,
    /// JSON Schema of the arguments.
    pub input_schema: Value,
    /// The full definition as the server sent it.
    pub raw: Value,
}

impl Tool {
    /// Parses a tool definition.
    pub fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            name: v.get("name")?.as_str()?.to_owned(),
            description: v
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            input_schema: v.get("inputSchema").cloned().unwrap_or_else(|| json!({})),
            raw: v.clone(),
        })
    }
}

/// The result of `tools/call`.
#[derive(Debug, Clone, PartialEq)]
pub struct CallToolResult {
    /// Content blocks (`text`, `image`, `resource`, ...).
    pub content: Vec<Value>,
    /// Structured output, if the tool returned any.
    pub structured_content: Option<Value>,
    /// True when the tool reported an error.
    pub is_error: bool,
}

impl CallToolResult {
    /// Parses a `CallToolResult`.
    pub fn from_value(v: &Value) -> Self {
        Self {
            content: v
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            structured_content: v.get("structuredContent").cloned(),
            is_error: v.get("isError").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    /// The concatenated `text` content blocks.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_messages() {
        let r = parse_incoming(&json!({"jsonrpc":"2.0","id":1,"result":{"x":1}})).unwrap();
        assert!(matches!(r, Incoming::Response { outcome: Ok(_), .. }));
        let e = parse_incoming(
            &json!({"jsonrpc":"2.0","id":"a","error":{"code":-32022,"message":"no","data":{"supported":["2025-11-25"]}}}),
        )
        .unwrap();
        let Incoming::Response {
            outcome: Err(err), ..
        } = e
        else {
            panic!()
        };
        assert_eq!(err.supported_versions(), vec!["2025-11-25"]);
        assert!(is_modern_error(err.code));
        let n =
            parse_incoming(&json!({"jsonrpc":"2.0","method":"notifications/progress"})).unwrap();
        assert!(matches!(n, Incoming::Notification { .. }));
        let q = parse_incoming(&json!({"jsonrpc":"2.0","id":7,"method":"roots/list"})).unwrap();
        assert!(matches!(q, Incoming::Request { .. }));
        let bad = parse_incoming(&json!({"jsonrpc":"2.0","id":3})).unwrap();
        let Incoming::Response {
            outcome: Err(err), ..
        } = bad
        else {
            panic!()
        };
        assert_eq!(err.code, INVALID_REQUEST);
        assert!(parse_incoming(&json!([1])).is_err());
        assert!(parse_incoming(&json!({"id":1,"result":{}})).is_err());
    }

    #[test]
    fn modern_meta_is_added() {
        let p = with_modern_meta(
            json!({"name":"t","_meta":{"progressToken":1}}),
            "2026-07-28",
        );
        assert_eq!(p["_meta"][META_PROTOCOL_VERSION], "2026-07-28");
        assert_eq!(p["_meta"]["progressToken"], 1);
        assert_eq!(p["_meta"][META_CLIENT_INFO]["name"], "harness");
        assert!(p["_meta"][META_CLIENT_CAPABILITIES].is_object());
        assert_eq!(p["name"], "t");
    }

    #[test]
    fn result_types() {
        assert!(check_result_type(&json!({})).is_ok());
        assert!(check_result_type(&json!({"resultType":"complete"})).is_ok());
        assert!(check_result_type(&json!({"resultType":"input_required"})).is_err());
        assert!(check_result_type(&json!({"resultType":"weird"})).is_err());
    }
}
