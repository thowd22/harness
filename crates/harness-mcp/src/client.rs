//! One live connection to an MCP server: era detection, version
//! negotiation and the requests harness makes.
//!
//! Opening a connection follows MCP 2026-07-28 "Versioning and
//! Compatibility" for a dual-era client:
//!
//! - **stdio**: probe with `server/discover`. A `DiscoverResult` means a
//!   modern server; a recognised modern error (such as
//!   `UnsupportedProtocolVersionError`) means a modern server with other
//!   versions; any other error or no answer within the probe timeout means
//!   a legacy server, which gets the `initialize` handshake.
//! - **HTTP**: send `server/discover` as a modern request. A `4xx` whose
//!   body is not a recognised modern JSON-RPC error means a legacy server.
//!
//! The legacy handshake is `initialize` (version and capability
//! negotiation) followed by `notifications/initialized`. Liveness checks use
//! `ping` on legacy servers and `server/discover` on modern ones (the
//! modern protocol removed `ping`).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

use crate::error::{McpError, Result};
use crate::headers::{self, ParamHeader};
use crate::http::HttpTransport;
use crate::prompts;
use crate::protocol::{
    self, CallToolResult, Era, LEGACY_VERSIONS, MODERN_PROTOCOL_VERSION, MODERN_VERSIONS,
    ServerInfo, Tool,
};
use crate::stdio::StdioTransport;

/// The transport under a connection.
pub enum Transport {
    /// A local subprocess.
    Stdio(StdioTransport),
    /// A remote streamable-HTTP endpoint.
    Http(HttpTransport),
}

impl Transport {
    async fn request(
        &self,
        method: &str,
        params: Value,
        extra: &[(String, String)],
        limit: Duration,
    ) -> Result<Value> {
        match self {
            Transport::Stdio(t) => t.request(method, params, limit).await,
            Transport::Http(t) => t.request(method, params, extra, limit).await,
        }
    }

    async fn notify(&self, method: &str, params: Value, limit: Duration) -> Result<()> {
        match self {
            Transport::Stdio(t) => t.notify(method, params).await,
            Transport::Http(t) => t.notify(method, params, limit).await,
        }
    }

    fn set_protocol(&self, era: Era, version: &str) {
        match self {
            Transport::Stdio(t) => t.set_legacy(era == Era::Legacy),
            Transport::Http(t) => t.set_protocol(era, version),
        }
    }
}

/// What the handshake established.
#[derive(Debug, Clone, PartialEq)]
pub struct Negotiated {
    /// Protocol generation.
    pub era: Era,
    /// Protocol version in use.
    pub version: String,
    /// Self-reported server identity.
    pub server_info: ServerInfo,
    /// Server capabilities.
    pub capabilities: Value,
    /// Server instructions for the model (untrusted text), if any.
    pub instructions: Option<String>,
}

/// Timeouts for one connection.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Per-request limit.
    pub request: Duration,
    /// stdio `server/discover` probe limit before assuming a legacy server.
    pub probe: Duration,
}

/// A connected MCP server.
pub struct Connection {
    server: String,
    transport: Transport,
    negotiated: Negotiated,
    limits: Limits,
    /// HTTP only: `x-mcp-header` annotations per tool, from `tools/list`.
    param_headers: Mutex<HashMap<String, Vec<ParamHeader>>>,
}

impl Connection {
    /// Detects the server's era and completes the handshake.
    pub async fn open(server: &str, transport: Transport, limits: Limits) -> Result<Self> {
        let negotiated = match &transport {
            Transport::Stdio(_) => handshake_stdio(server, &transport, limits).await?,
            Transport::Http(_) => handshake_http(server, &transport, limits).await?,
        };
        tracing::info!(
            server,
            era = negotiated.era.as_str(),
            version = %negotiated.version,
            server_name = %negotiated.server_info.name,
            "MCP server connected"
        );
        Ok(Self {
            server: server.into(),
            transport,
            negotiated,
            limits,
            param_headers: Mutex::default(),
        })
    }

    /// What the handshake established.
    pub fn negotiated(&self) -> &Negotiated {
        &self.negotiated
    }

    /// The transport.
    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    /// True while the connection can be used (stdio: the process runs).
    pub fn is_alive(&self) -> bool {
        match &self.transport {
            Transport::Stdio(t) => t.is_alive(),
            Transport::Http(_) => true,
        }
    }

    /// Why a stdio connection ended, if it did.
    pub fn death(&self) -> Option<McpError> {
        match &self.transport {
            Transport::Stdio(t) => t.death(),
            Transport::Http(_) => None,
        }
    }

    /// Sends `method` with the era's metadata and returns the final result.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_with(method, params, &[]).await
    }

    async fn call_with(
        &self,
        method: &str,
        params: Value,
        extra: &[(String, String)],
    ) -> Result<Value> {
        let params = match self.negotiated.era {
            Era::Modern => protocol::with_modern_meta(params, &self.negotiated.version),
            Era::Legacy => params,
        };
        let result = self
            .transport
            .request(method, params, extra, self.limits.request)
            .await?;
        protocol::check_result_type(&result).map_err(|message| McpError::Protocol {
            server: self.server.clone(),
            message: format!("`{method}`: {message}"),
        })?;
        Ok(result)
    }

    /// Checks the server answers: `ping` (legacy) or `server/discover`
    /// (modern, which has no `ping`).
    pub async fn ping(&self) -> Result<()> {
        match self.negotiated.era {
            Era::Legacy => self.call("ping", json!({})).await.map(|_| ()),
            Era::Modern => self.call("server/discover", json!({})).await.map(|_| ()),
        }
    }

    /// Lists every tool, following `nextCursor`. Over HTTP, tools whose
    /// `x-mcp-header` annotations are invalid are left out (with a
    /// warning), as the spec requires.
    pub async fn list_tools(&self) -> Result<Vec<Tool>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..1000 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let result = self.call("tools/list", params).await?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| McpError::Protocol {
                    server: self.server.clone(),
                    message: "`tools/list` result has no `tools` array".into(),
                })?;
            for t in page {
                match Tool::from_value(t) {
                    Some(tool) => tools.push(tool),
                    None => {
                        tracing::warn!(server = %self.server, "MCP tool without a name; skipped")
                    }
                }
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        if matches!(self.transport, Transport::Http(_)) {
            let mut map = HashMap::new();
            tools.retain(|t| match headers::param_headers(&t.input_schema) {
                Ok(h) => {
                    map.insert(t.name.clone(), h);
                    true
                }
                Err(why) => {
                    tracing::warn!(server = %self.server, tool = %t.name, why = %why, "MCP tool definition rejected");
                    false
                }
            });
            if let Ok(mut p) = self.param_headers.lock() {
                *p = map;
            }
        }
        Ok(tools)
    }

    /// Calls tool `name` with `arguments`.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult> {
        let mut extra = Vec::new();
        if matches!(self.transport, Transport::Http(_)) && self.negotiated.era == Era::Modern {
            extra.push(("Mcp-Name".to_owned(), headers::encode_value(name)));
            let known = self
                .param_headers
                .lock()
                .ok()
                .and_then(|p| p.get(name).cloned());
            if let Some(h) = known {
                extra.extend(headers::param_header_values(&h, &arguments));
            }
        }
        let result = self
            .call_with(
                "tools/call",
                json!({"name": name, "arguments": arguments}),
                &extra,
            )
            .await?;
        Ok(CallToolResult::from_value(&result))
    }

    /// `Mcp-Name` for a modern HTTP request naming `name`.
    fn name_header(&self, name: &str) -> Vec<(String, String)> {
        if matches!(self.transport, Transport::Http(_)) && self.negotiated.era == Era::Modern {
            vec![("Mcp-Name".to_owned(), headers::encode_value(name))]
        } else {
            Vec::new()
        }
    }

    /// Reads every page of a `*/list` method: the items of `key`, parsed
    /// with `parse`, at most [`prompts::MAX_LIST_ITEMS`].
    async fn list_all<T>(
        &self,
        method: &str,
        key: &str,
        parse: impl Fn(&Value) -> Option<T>,
    ) -> Result<Vec<T>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..1000 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let result = self.call(method, params).await?;
            let page =
                result
                    .get(key)
                    .and_then(Value::as_array)
                    .ok_or_else(|| McpError::Protocol {
                        server: self.server.clone(),
                        message: format!("`{method}` result has no `{key}` array"),
                    })?;
            out.extend(page.iter().filter_map(&parse));
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() || out.len() >= prompts::MAX_LIST_ITEMS {
                break;
            }
        }
        out.truncate(prompts::MAX_LIST_ITEMS);
        Ok(out)
    }

    /// Lists the server's prompts (empty when it declares no `prompts`
    /// capability).
    pub async fn list_prompts(&self) -> Result<Vec<prompts::Prompt>> {
        if !prompts::has_prompts(&self.negotiated.capabilities) {
            return Ok(Vec::new());
        }
        self.list_all("prompts/list", "prompts", prompts::Prompt::from_value)
            .await
    }

    /// Gets prompt `name` with string `arguments`.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: &std::collections::BTreeMap<String, String>,
    ) -> Result<prompts::GetPromptResult> {
        let result = self
            .call_with(
                "prompts/get",
                json!({"name": name, "arguments": arguments}),
                &self.name_header(name),
            )
            .await?;
        Ok(prompts::GetPromptResult::from_value(&result))
    }

    /// Lists the server's resources (empty when it declares no
    /// `resources` capability).
    pub async fn list_resources(&self) -> Result<Vec<prompts::Resource>> {
        if !prompts::has_resources(&self.negotiated.capabilities) {
            return Ok(Vec::new());
        }
        self.list_all("resources/list", "resources", prompts::Resource::from_value)
            .await
    }

    /// Reads resource `uri`.
    pub async fn read_resource(&self, uri: &str) -> Result<prompts::ReadResourceResult> {
        let result = self
            .call_with(
                "resources/read",
                json!({"uri": uri}),
                &self.name_header(uri),
            )
            .await?;
        Ok(prompts::ReadResourceResult::from_value(&result))
    }

    /// Notifications the server sends from now on.
    pub fn notifications(&self) -> tokio::sync::broadcast::Receiver<protocol::Notification> {
        match &self.transport {
            Transport::Stdio(t) => t.subscribe(),
            Transport::Http(t) => t.subscribe(),
        }
    }

    /// True when the server announces tool-list changes
    /// (`capabilities.tools.listChanged`).
    pub fn announces_tool_changes(&self) -> bool {
        protocol::tools_list_changed(&self.negotiated.capabilities)
    }

    /// Asks the server for tool-list change notifications and keeps the
    /// stream open until it ends:
    ///
    /// - stdio, modern: sends `subscriptions/listen` and returns at once
    ///   (notifications arrive on stdout for the life of the process);
    /// - stdio, legacy: nothing to do (servers notify unasked);
    /// - HTTP, modern: holds a `subscriptions/listen` stream open;
    /// - HTTP, legacy: holds the `GET` stream open.
    ///
    /// Returns `Ok(true)` when the caller should open it again after it
    /// ends (HTTP), `Ok(false)` when nothing more is needed.
    pub async fn listen_for_changes(&self) -> Result<bool> {
        let version = &self.negotiated.version;
        match (&self.transport, self.negotiated.era) {
            (Transport::Stdio(t), Era::Modern) => {
                t.open_stream(
                    "subscriptions/listen",
                    protocol::with_modern_meta(protocol::listen_params(), version),
                )
                .await?;
                Ok(false)
            }
            (Transport::Stdio(_), Era::Legacy) => Ok(false),
            (Transport::Http(t), Era::Modern) => {
                t.listen(protocol::with_modern_meta(
                    protocol::listen_params(),
                    version,
                ))
                .await?;
                Ok(true)
            }
            (Transport::Http(t), Era::Legacy) => {
                t.listen_get().await?;
                Ok(true)
            }
        }
    }

    /// Ends the connection (stdio: close stdin, then signals; HTTP legacy:
    /// `DELETE` the session).
    pub async fn shutdown(&self, grace: Duration) {
        match &self.transport {
            Transport::Stdio(t) => t.shutdown(grace).await,
            Transport::Http(t) => t.shutdown(grace).await,
        }
    }
}

fn negotiated_from_discover(result: &Value, version: String) -> Negotiated {
    Negotiated {
        era: Era::Modern,
        version,
        server_info: ServerInfo::from_value(
            result
                .get("_meta")
                .and_then(|m| m.get(protocol::META_SERVER_INFO)),
        ),
        capabilities: result.get("capabilities").cloned().unwrap_or(json!({})),
        instructions: result
            .get("instructions")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

/// Picks a modern version from a server's list.
fn pick_modern(supported: &[String]) -> Option<String> {
    MODERN_VERSIONS
        .iter()
        .find(|v| supported.iter().any(|s| s == *v))
        .map(|v| (*v).to_owned())
}

/// Picks a legacy version from a server's list.
fn pick_legacy(supported: &[String]) -> Option<String> {
    LEGACY_VERSIONS
        .iter()
        .find(|v| supported.iter().any(|s| s == *v))
        .map(|v| (*v).to_owned())
}

fn no_common(server: &str, supported: Vec<String>) -> McpError {
    McpError::NoCommonVersion {
        server: server.into(),
        supported,
        ours: MODERN_VERSIONS
            .iter()
            .chain(LEGACY_VERSIONS)
            .map(|v| (*v).to_owned())
            .collect(),
    }
}

/// Sends `server/discover` with `version` in `_meta` (and headers).
async fn discover(transport: &Transport, version: &str, limit: Duration) -> Result<Value> {
    transport.set_protocol(Era::Modern, version);
    transport
        .request(
            "server/discover",
            protocol::with_modern_meta(json!({}), version),
            &[],
            limit,
        )
        .await
}

/// Handles a `DiscoverResult` (or a version error) once the server is
/// known to be modern.
async fn finish_modern(
    server: &str,
    transport: &Transport,
    first: Result<Value>,
    limits: Limits,
) -> Result<Negotiated> {
    let result = match first {
        Ok(r) => r,
        Err(e) => {
            let supported = e
                .rpc_error()
                .map(|r| r.supported_versions())
                .unwrap_or_default();
            if let Some(v) = pick_modern(&supported).filter(|v| v != MODERN_PROTOCOL_VERSION) {
                discover(transport, &v, limits.request).await?
            } else if let Some(v) = pick_legacy(&supported) {
                return legacy_initialize(server, transport, &v, limits).await;
            } else {
                return Err(no_common(server, supported));
            }
        }
    };
    let supported: Vec<String> = result
        .get("supportedVersions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    match pick_modern(&supported) {
        Some(v) => {
            transport.set_protocol(Era::Modern, &v);
            Ok(negotiated_from_discover(&result, v))
        }
        None => match pick_legacy(&supported) {
            Some(v) => legacy_initialize(server, transport, &v, limits).await,
            None => Err(no_common(server, supported)),
        },
    }
}

async fn handshake_stdio(
    server: &str,
    transport: &Transport,
    limits: Limits,
) -> Result<Negotiated> {
    let probe = discover(transport, MODERN_PROTOCOL_VERSION, limits.probe).await;
    let modern = match &probe {
        Ok(_) => true,
        Err(e) => e
            .rpc_error()
            .is_some_and(|r| protocol::is_modern_error(r.code)),
    };
    if modern {
        return finish_modern(server, transport, probe, limits).await;
    }
    match probe {
        Err(e) if e.is_connection_lost() => Err(e),
        Err(e) => {
            tracing::debug!(server, probe = %e, "no modern answer to server/discover; trying the legacy handshake");
            legacy_initialize(server, transport, LEGACY_VERSIONS[0], limits).await
        }
        Ok(_) => unreachable!("handled above"),
    }
}

async fn handshake_http(server: &str, transport: &Transport, limits: Limits) -> Result<Negotiated> {
    let probe = discover(transport, MODERN_PROTOCOL_VERSION, limits.request).await;
    let legacy = match &probe {
        Ok(_) => Some(false),
        Err(McpError::Http {
            status: Some(s),
            rpc,
            ..
        }) if (400..500).contains(s) => Some(
            !rpc.as_ref()
                .is_some_and(|r| protocol::is_modern_error(r.code)),
        ),
        // A JSON-RPC error inside a 200: modern only if it is a modern code.
        Err(McpError::Rpc { error, .. }) => Some(!protocol::is_modern_error(error.code)),
        Err(_) => None,
    };
    let Some(legacy) = legacy else {
        return Err(probe.expect_err("only errors reach here"));
    };
    if legacy {
        tracing::debug!(
            server,
            "server/discover rejected; trying the legacy handshake"
        );
        legacy_initialize(server, transport, LEGACY_VERSIONS[0], limits).await
    } else {
        finish_modern(server, transport, probe, limits).await
    }
}

async fn legacy_initialize(
    server: &str,
    transport: &Transport,
    version: &str,
    limits: Limits,
) -> Result<Negotiated> {
    if let Transport::Http(t) = transport {
        t.clear_session();
    }
    transport.set_protocol(Era::Legacy, version);
    let result = transport
        .request(
            "initialize",
            json!({
                "protocolVersion": version,
                "capabilities": protocol::client_capabilities(),
                "clientInfo": protocol::client_info(),
            }),
            &[],
            limits.request,
        )
        .await?;
    let chosen = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if !LEGACY_VERSIONS.contains(&chosen.as_str()) {
        return Err(no_common(server, vec![chosen]));
    }
    transport.set_protocol(Era::Legacy, &chosen);
    transport
        .notify("notifications/initialized", json!({}), limits.request)
        .await?;
    Ok(Negotiated {
        era: Era::Legacy,
        version: chosen,
        server_info: ServerInfo::from_value(result.get("serverInfo")),
        capabilities: result.get("capabilities").cloned().unwrap_or(json!({})),
        instructions: result
            .get("instructions")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}
