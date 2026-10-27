//! Finding the authorization server of an MCP server: the `401` challenge,
//! Protected Resource Metadata (RFC 9728) and Authorization Server
//! Metadata (RFC 8414 / OpenID Connect Discovery), in the order the MCP
//! spec requires.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{AuthError, canonical_resource, secure_endpoint};
use crate::protocol::{self, MODERN_PROTOCOL_VERSION};

/// Largest metadata document accepted (bytes).
const MAX_DOC: usize = 256 * 1024;

/// A parsed `WWW-Authenticate: Bearer …` challenge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Challenge {
    /// `resource_metadata`: where the Protected Resource Metadata is.
    pub resource_metadata: Option<String>,
    /// `scope`: the scopes this request needs.
    pub scope: Option<String>,
    /// `error` (`invalid_token`, `insufficient_scope`, …).
    pub error: Option<String>,
}

/// Parses the `Bearer` challenge out of `WWW-Authenticate` header values
/// (which may hold several challenges). `None` without a `Bearer` one.
pub fn parse_www_authenticate<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<Challenge> {
    for value in values {
        let mut rest = value;
        while !rest.is_empty() {
            let (scheme, after) = split_token(rest.trim_start());
            if scheme.is_empty() {
                break;
            }
            let (params, next) = parse_params(after);
            if scheme.eq_ignore_ascii_case("bearer") {
                let get = |k: &str| {
                    params
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case(k))
                        .map(|(_, v)| v.clone())
                };
                return Some(Challenge {
                    resource_metadata: get("resource_metadata"),
                    scope: get("scope"),
                    error: get("error"),
                });
            }
            rest = next;
        }
    }
    None
}

/// Splits a leading token (up to whitespace, `=` or `,`).
fn split_token(s: &str) -> (&str, &str) {
    let end = s
        .find(|c: char| c.is_whitespace() || c == '=' || c == ',')
        .unwrap_or(s.len());
    (&s[..end], &s[end..])
}

/// Reads `k=v, k="v"` pairs until the next challenge's scheme.
fn parse_params(mut s: &str) -> (Vec<(String, String)>, &str) {
    let mut out = Vec::new();
    loop {
        s = s.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
        let (key, after) = split_token(s);
        let after_ws = after.trim_start();
        if key.is_empty() || !after_ws.starts_with('=') {
            // A new scheme (or the end).
            return (out, s);
        }
        let v = after_ws[1..].trim_start();
        let (value, rest) = if let Some(q) = v.strip_prefix('"') {
            let mut val = String::new();
            let mut chars = q.char_indices();
            let mut end = q.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, n)) = chars.next() {
                            val.push(n);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    c => val.push(c),
                }
            }
            (val, &q[end.min(q.len())..])
        } else {
            let end = v
                .find(|c: char| c == ',' || c.is_whitespace())
                .unwrap_or(v.len());
            (v[..end].to_owned(), &v[end..])
        };
        out.push((key.to_owned(), value));
        s = rest;
    }
}

/// Protected Resource Metadata (RFC 9728), the parts harness uses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProtectedResource {
    /// The resource identifier.
    pub resource: String,
    /// Authorization servers (issuer identifiers).
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    /// Scopes for basic use.
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
}

/// Authorization Server Metadata (RFC 8414), the parts harness uses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AuthServer {
    /// The issuer identifier.
    pub issuer: String,
    /// Authorization endpoint.
    pub authorization_endpoint: String,
    /// Token endpoint.
    pub token_endpoint: String,
    /// Dynamic Client Registration endpoint, if supported.
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    /// Supported scopes.
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
    /// PKCE methods; harness requires `S256`.
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
    /// Client ID Metadata Documents are accepted.
    #[serde(default)]
    pub client_id_metadata_document_supported: bool,
    /// The authorization response carries `iss` (RFC 9207).
    #[serde(default)]
    pub authorization_response_iss_parameter_supported: bool,
    /// Device Authorization endpoint (RFC 8628), if the server offers the
    /// device grant.
    #[serde(default)]
    pub device_authorization_endpoint: Option<String>,
}

/// Sends an unauthenticated MCP request (`server/discover`) to `url` and
/// returns the `Bearer` challenge of a `401`, if any. Other answers mean
/// the server did not ask for authorization on this request.
pub async fn probe(http: &reqwest::Client, url: &str) -> Result<Option<Challenge>, AuthError> {
    let body = protocol::request(
        0,
        "server/discover",
        protocol::with_modern_meta(json!({}), MODERN_PROTOCOL_VERSION),
    );
    let resp = http
        .post(url)
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .header("MCP-Protocol-Version", MODERN_PROTOCOL_VERSION)
        .header("Mcp-Method", "server/discover")
        .body(serde_json::to_vec(&body).expect("JSON"))
        .send()
        .await
        .map_err(|e| AuthError::Network(format!("{url}: {}", e.without_url())))?;
    if resp.status().as_u16() != 401 {
        return Ok(None);
    }
    let values: Vec<String> = resp
        .headers()
        .get_all("www-authenticate")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_owned))
        .collect();
    Ok(Some(
        parse_www_authenticate(values.iter().map(String::as_str)).unwrap_or_default(),
    ))
}

/// The well-known Protected Resource Metadata URLs for `server_url`, in
/// the order to try: path-inserted, then root.
pub fn resource_metadata_urls(server_url: &str) -> Vec<String> {
    let Ok(u) = reqwest::Url::parse(server_url) else {
        return Vec::new();
    };
    let origin = u.origin().ascii_serialization();
    let path = u.path().trim_end_matches('/');
    let mut out = Vec::new();
    if !path.is_empty() {
        out.push(format!(
            "{origin}/.well-known/oauth-protected-resource{path}"
        ));
    }
    out.push(format!("{origin}/.well-known/oauth-protected-resource"));
    out
}

/// The Authorization Server Metadata URLs for `issuer`, in the spec's
/// order.
pub fn auth_server_metadata_urls(issuer: &str) -> Vec<String> {
    let Ok(u) = reqwest::Url::parse(issuer) else {
        return Vec::new();
    };
    let origin = u.origin().ascii_serialization();
    let path = u.path().trim_end_matches('/');
    if path.is_empty() {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server"),
            format!("{origin}/.well-known/openid-configuration"),
        ]
    } else {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server{path}"),
            format!("{origin}/.well-known/openid-configuration{path}"),
            format!("{origin}{path}/.well-known/openid-configuration"),
        ]
    }
}

/// GETs a JSON document; `Ok(None)` for a 4xx (try the next URL).
async fn fetch_json(http: &reqwest::Client, url: &str) -> Result<Option<Value>, AuthError> {
    let resp = http
        .get(url)
        .header("Accept", "application/json")
        .header("MCP-Protocol-Version", MODERN_PROTOCOL_VERSION)
        .send()
        .await
        .map_err(|e| AuthError::Network(format!("{url}: {}", e.without_url())))?;
    let status = resp.status();
    if status.is_client_error() || status.is_redirection() {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(AuthError::Network(format!(
            "{url} answered HTTP {}",
            status.as_u16()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AuthError::Network(format!("{url}: {}", e.without_url())))?;
    if bytes.len() > MAX_DOC {
        return Err(AuthError::Discovery(format!(
            "{url}: the document is too large"
        )));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| AuthError::Discovery(format!("{url} is not a JSON metadata document")))
}

/// True when `resource` names the server at `server_url`: the same
/// canonical URL, or the server's origin.
fn resource_matches(resource: &str, server_url: &str) -> bool {
    let (Some(r), Some(s)) = (canonical_resource(resource), canonical_resource(server_url)) else {
        return false;
    };
    if r == s {
        return true;
    }
    reqwest::Url::parse(server_url)
        .is_ok_and(|u| canonical_resource(&u.origin().ascii_serialization()).as_deref() == Some(&r))
}

/// The Protected Resource Metadata of the server at `server_url`: from the
/// challenge's `resource_metadata`, else the well-known URLs. Its
/// `resource` must name this server and it must list an authorization
/// server.
pub async fn protected_resource(
    http: &reqwest::Client,
    server_url: &str,
    challenge: Option<&Challenge>,
) -> Result<ProtectedResource, AuthError> {
    let urls = match challenge.and_then(|c| c.resource_metadata.clone()) {
        Some(u) => vec![u],
        None => resource_metadata_urls(server_url),
    };
    let mut found = None;
    for url in &urls {
        // Over plain http an attacker could swap the metadata and point
        // harness at their own authorization server, so even the server's
        // own origin must be TLS (or loopback).
        if !secure_endpoint(url) {
            continue;
        }
        if let Some(v) = fetch_json(http, url).await? {
            found = Some((url.clone(), v));
            break;
        }
    }
    let Some((url, doc)) = found else {
        return Err(AuthError::Discovery(format!(
            "the server publishes no OAuth protected resource metadata (tried {}); it may not support OAuth. Use header.* credentials and auth = none instead",
            urls.join(", ")
        )));
    };
    let prm: ProtectedResource = serde_json::from_value(doc).map_err(|e| {
        AuthError::Discovery(format!(
            "{url}: not valid protected resource metadata ({e})"
        ))
    })?;
    if !resource_matches(&prm.resource, server_url) {
        return Err(AuthError::Discovery(format!(
            "{url} describes `{}`, not {server_url}; refusing to authorize",
            prm.resource
        )));
    }
    if prm.authorization_servers.is_empty() {
        return Err(AuthError::Discovery(format!(
            "{url} lists no authorization server"
        )));
    }
    Ok(prm)
}

/// The metadata of the authorization server `issuer`, tried in the spec's
/// order. The document's `issuer` must equal `issuer` exactly, its
/// endpoints must be HTTPS (or loopback HTTP), and it must support PKCE
/// `S256`.
pub async fn auth_server(http: &reqwest::Client, issuer: &str) -> Result<AuthServer, AuthError> {
    if !secure_endpoint(issuer) {
        return Err(AuthError::Discovery(format!(
            "the authorization server `{issuer}` is not an https:// URL; refusing to authorize"
        )));
    }
    let urls = auth_server_metadata_urls(issuer);
    for url in &urls {
        let Some(doc) = fetch_json(http, url).await? else {
            continue;
        };
        let meta: AuthServer = serde_json::from_value(doc).map_err(|e| {
            AuthError::Discovery(format!(
                "{url}: not valid authorization server metadata ({e})"
            ))
        })?;
        if meta.issuer != issuer {
            return Err(AuthError::Discovery(format!(
                "{url} claims issuer `{}`, not `{issuer}`; the metadata was not used",
                meta.issuer
            )));
        }
        for e in [&meta.authorization_endpoint, &meta.token_endpoint]
            .into_iter()
            .chain(meta.registration_endpoint.as_ref())
            .chain(meta.device_authorization_endpoint.as_ref())
        {
            if !secure_endpoint(e) {
                return Err(AuthError::Discovery(format!(
                    "the authorization server endpoint `{e}` is not an https:// URL; refusing to authorize"
                )));
            }
        }
        if !meta
            .code_challenge_methods_supported
            .iter()
            .any(|m| m == "S256")
        {
            return Err(AuthError::Discovery(format!(
                "the authorization server `{issuer}` does not advertise PKCE S256 support (code_challenge_methods_supported); harness will not authorize without it"
            )));
        }
        return Ok(meta);
    }
    Err(AuthError::Discovery(format!(
        "no authorization server metadata for `{issuer}` (tried {})",
        urls.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bearer_challenges() {
        let c = parse_www_authenticate([
            r#"Bearer resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource", scope="files:read files:write""#,
        ])
        .unwrap();
        assert_eq!(
            c.resource_metadata.as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(c.scope.as_deref(), Some("files:read files:write"));
        let c = parse_www_authenticate([
            r#"Basic realm="x", Bearer error="insufficient_scope", scope="a\"b", error_description="more""#,
        ])
        .unwrap();
        assert_eq!(c.error.as_deref(), Some("insufficient_scope"));
        assert_eq!(c.scope.as_deref(), Some("a\"b"));
        let c = parse_www_authenticate(["bearer scope=read"]).unwrap();
        assert_eq!(c.scope.as_deref(), Some("read"));
        assert!(parse_www_authenticate([r#"Basic realm="x""#]).is_none());
        assert_eq!(
            parse_www_authenticate(["Bearer"]),
            Some(Challenge::default())
        );
    }

    #[test]
    fn well_known_urls_follow_the_spec_order() {
        assert_eq!(
            resource_metadata_urls("https://example.com/public/mcp"),
            [
                "https://example.com/.well-known/oauth-protected-resource/public/mcp",
                "https://example.com/.well-known/oauth-protected-resource",
            ]
        );
        assert_eq!(
            resource_metadata_urls("https://example.com"),
            ["https://example.com/.well-known/oauth-protected-resource"]
        );
        assert_eq!(
            auth_server_metadata_urls("https://auth.example.com/tenant1"),
            [
                "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
                "https://auth.example.com/.well-known/openid-configuration/tenant1",
                "https://auth.example.com/tenant1/.well-known/openid-configuration",
            ]
        );
        assert_eq!(
            auth_server_metadata_urls("https://auth.example.com"),
            [
                "https://auth.example.com/.well-known/oauth-authorization-server",
                "https://auth.example.com/.well-known/openid-configuration",
            ]
        );
    }

    #[test]
    fn resource_must_name_the_server() {
        assert!(resource_matches(
            "https://mcp.example.com/mcp",
            "https://MCP.example.com/mcp"
        ));
        assert!(resource_matches(
            "https://mcp.example.com",
            "https://mcp.example.com/mcp"
        ));
        assert!(!resource_matches(
            "https://evil.example.com/mcp",
            "https://mcp.example.com/mcp"
        ));
        assert!(!resource_matches(
            "https://mcp.example.com/other",
            "https://mcp.example.com/mcp"
        ));
    }
}
