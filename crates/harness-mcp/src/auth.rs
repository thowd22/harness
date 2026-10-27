//! OAuth for remote MCP servers, following the MCP authorization spec
//! (<https://modelcontextprotocol.io/specification>, "Authorization").
//!
//! A remote server with `auth = oauth` in its `[mcp.<name>]` section gets
//! its access token from the protected token store. The browser flow runs
//! only from `harness login mcp <name>` and the TUI `/login mcp <name>`
//! ([`flow::Authorize`]); runs (TUI turns, `harness run`, workflows, evals)
//! never open a browser and fail with a message that says to authorize
//! first.
//!
//! The flow:
//!
//! 1. [`discovery`]: an unauthenticated request to the MCP endpoint; the
//!    `401` carries `WWW-Authenticate: Bearer resource_metadata=…, scope=…`.
//!    The Protected Resource Metadata (RFC 9728) comes from that URL, else
//!    from the well-known URIs (path-inserted, then root); its `resource`
//!    must name this server. The first listed authorization server's
//!    metadata comes from the RFC 8414 / OpenID Connect well-known URLs in
//!    the spec's order, and its `issuer` must match exactly. Servers that do
//!    not advertise PKCE `S256` are refused.
//! 2. Client registration in the spec's priority order: `oauth_client_id`
//!    (pre-registered), `oauth_client_metadata_url` when the server supports
//!    Client ID Metadata Documents, else Dynamic Client Registration
//!    (RFC 7591, `token_endpoint_auth_method = none`,
//!    `application_type = native`). A registration is bound to the issuer
//!    that issued it and reused for that issuer only.
//! 3. Authorization code with PKCE (`S256`), a fresh `state`, the `resource`
//!    parameter (RFC 8707) and a loopback redirect
//!    (`http://127.0.0.1:<port>/callback`). The callback's `iss` is checked
//!    as RFC 9207 requires before the code goes anywhere.
//!    When the browser is on another machine (SSH, no display, WSL) and
//!    the authorization server advertises a
//!    `device_authorization_endpoint`, the device grant (RFC 8628,
//!    [`device`]) replaces the loopback redirect: the user confirms a
//!    short code on any device and harness polls the token endpoint. A
//!    server that refuses the device grant for this client gets the
//!    loopback flow instead.
//! 4. Tokens go to [`store::McpTokenStore`]: one JSON file per server under
//!    `<data_dir>/auth/mcp/`, mode `0600` in `0700` directories, inside the
//!    data dir's protected `auth/` directory, so agent tools can never read
//!    them and nothing syncs them to remote hosts. Tokens are
//!    [`Token`]s: zeroized on drop, `[redacted]` in `Debug`.
//! 5. [`session::OAuthSession`] adds `Authorization: Bearer …` to every
//!    request, refreshes (rotating the refresh token) shortly before expiry
//!    and once after a `401`, serialized per server so concurrent agents
//!    never spend a refresh token twice. A dead refresh token clears the
//!    stored tokens and asks to authorize again.

pub mod device;
pub mod discovery;
pub mod flow;
pub mod mock;
pub mod session;
pub mod store;

use std::time::Duration;

pub use flow::{Authorize, Authorized};
pub use harness_providers::chatgpt::store::Token;
pub use harness_providers::chatgpt::{Browser, SystemBrowser};
pub use session::OAuthSession;
pub use store::{ClientRegistration, McpAuthRecord, McpTokenStore, McpTokens, Registration};

/// The redirect path on the loopback listener.
pub const CALLBACK_PATH: &str = "/callback";

/// How long [`Authorize`] waits for the browser.
pub const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(300);

/// Refresh this long before the access token expires.
pub const REFRESH_SKEW: Duration = Duration::from_secs(60);

/// The client name sent with Dynamic Client Registration.
pub const CLIENT_NAME: &str = "harness";

/// Refresh error codes that mean the grant can never work again.
pub const TERMINAL_REFRESH_CODES: &[&str] =
    &["invalid_grant", "invalid_client", "unauthorized_client"];

/// The command that authorizes `server`, for messages.
pub fn login_command(server: &str) -> String {
    format!("harness login mcp {server}")
}

/// Why authorizing, refreshing or reading the token store failed. Messages
/// are short, say what to do next and never contain tokens.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// No usable tokens are stored; the browser flow has to run first.
    #[error(
        "needs OAuth authorization ({reason}); run `harness login mcp {server}` (in the TUI: /login mcp {server}) first. Runs never open a browser"
    )]
    NotAuthorized {
        /// Server name.
        server: String,
        /// Why.
        reason: String,
    },
    /// The refresh token is expired or revoked; stored tokens were cleared.
    #[error(
        "the OAuth authorization expired or was revoked ({reason}); run `harness login mcp {server}` to authorize again"
    )]
    ReauthRequired {
        /// Server name.
        server: String,
        /// The refresh error, or what happened.
        reason: String,
    },
    /// The user declined on the consent page.
    #[error("authorization was declined in the browser; nothing was saved")]
    Denied,
    /// The callback's `state` did not match this attempt.
    #[error("the authorization callback did not match this attempt (state mismatch); try again")]
    StateMismatch,
    /// The callback's `iss` did not match the authorization server (RFC 9207).
    #[error(
        "the authorization callback came from another issuer than `{0}` (possible mix-up attack); nothing was saved"
    )]
    IssuerMismatch(String),
    /// The device code (RFC 8628) expired before the user approved it.
    #[error(
        "the sign-in code expired before it was approved; run the sign-in again and approve it sooner"
    )]
    DeviceExpired,
    /// The browser did not come back in time.
    #[error("no answer from the browser within {}s; try again", .0.as_secs())]
    Timeout(Duration),
    /// The loopback listener could not be opened.
    #[error("cannot listen on 127.0.0.1 for the authorization callback ({0})")]
    Port(String),
    /// The browser could not be opened.
    #[error("could not open a browser ({0}); open this URL yourself: {1}")]
    Browser(String, String),
    /// The authorization server answered with an OAuth error.
    #[error("authorization failed: {code}{}", if .message.is_empty() { String::new() } else { format!(" ({})", .message) })]
    OAuth {
        /// OAuth `error` code.
        code: String,
        /// `error_description`, if any.
        message: String,
    },
    /// Finding the authorization server failed.
    #[error("{0}")]
    Discovery(String),
    /// No way to obtain a client id.
    #[error("{0}")]
    Registration(String),
    /// A network or server problem; nothing was changed.
    #[error("could not reach the authorization server: {0}")]
    Network(String),
    /// Reading or writing the token store failed.
    #[error("MCP token store: {0}")]
    Store(String),
    /// The server URL is plain `http://` on a non-loopback host, where
    /// bearer tokens would travel unencrypted (RFC 6750 section 5.3).
    #[error(
        "[mcp.{server}] url {url} is not https://; OAuth bearer tokens are sent only over TLS (plain http:// only to a loopback host such as 127.0.0.1). Change url to https://"
    )]
    InsecureUrl {
        /// Server name.
        server: String,
        /// The configured URL.
        url: String,
    },
}

/// Seconds since the Unix epoch.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Percent-encodes `s` (RFC 3986 unreserved characters stay as they are).
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `application/x-www-form-urlencoded` body (also a query string).
pub(crate) fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Parses `a=1&b=2` into decoded pairs.
pub(crate) fn parse_query(q: &str) -> Vec<(String, String)> {
    reqwest::Url::parse(&format!("http://x/?{q}"))
        .map(|u| u.query_pairs().into_owned().collect())
        .unwrap_or_default()
}

/// The canonical form of a server URL (RFC 8707 resource identifier):
/// lowercase scheme and host (the URL parser does that), no fragment, and
/// no trailing slash on an empty path. `None` for a URL that cannot be
/// parsed.
pub fn canonical_resource(url: &str) -> Option<String> {
    let mut u = reqwest::Url::parse(url).ok()?;
    u.set_fragment(None);
    let s = u.to_string();
    Some(if u.path() == "/" && u.query().is_none() {
        s.trim_end_matches('/').to_owned()
    } else {
        s
    })
}

/// True when `url` is `https://`, or `http://` on a loopback host (local
/// servers and tests). OAuth endpoints, and every server that receives a
/// bearer token, must be one of these.
pub(crate) fn secure_endpoint(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    match u.scheme() {
        "https" => true,
        "http" => matches!(
            u.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        ),
        _ => false,
    }
}

/// A client for OAuth calls: no redirects, 30 s limit.
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("HTTP client")
}

/// Splits a space-separated scope string.
pub(crate) fn scopes_of(scope: &str) -> Vec<String> {
    scope.split_whitespace().map(str::to_owned).collect()
}

/// Adds the items of `more` that `into` lacks, keeping order.
pub(crate) fn union_into(into: &mut Vec<String>, more: &[String]) {
    for s in more {
        if !into.contains(s) {
            into.push(s.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_resources() {
        assert_eq!(
            canonical_resource("HTTPS://MCP.Example.com/").as_deref(),
            Some("https://mcp.example.com")
        );
        assert_eq!(
            canonical_resource("https://mcp.example.com/mcp#frag").as_deref(),
            Some("https://mcp.example.com/mcp")
        );
        assert_eq!(
            canonical_resource("https://mcp.example.com:8443").as_deref(),
            Some("https://mcp.example.com:8443")
        );
        assert!(canonical_resource("mcp.example.com").is_none());
    }

    #[test]
    fn endpoints_must_be_https_or_loopback() {
        assert!(secure_endpoint("https://auth.example.com/token"));
        assert!(secure_endpoint("http://127.0.0.1:9/token"));
        assert!(secure_endpoint("http://localhost:9/token"));
        assert!(!secure_endpoint("http://auth.example.com/token"));
        assert!(!secure_endpoint("ftp://auth.example.com"));
    }

    #[test]
    fn form_and_query_round_trip() {
        let q = form_encode(&[
            ("scope", "a b"),
            ("redirect_uri", "http://127.0.0.1:1/callback"),
        ]);
        assert_eq!(
            q,
            "scope=a%20b&redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2Fcallback"
        );
        let back = parse_query(&q);
        assert_eq!(back[0], ("scope".into(), "a b".into()));
        assert_eq!(back[1].1, "http://127.0.0.1:1/callback");
        let mut v = vec!["a".to_owned()];
        union_into(&mut v, &["b".into(), "a".into()]);
        assert_eq!(v, ["a", "b"]);
    }

    #[test]
    fn messages_say_how_to_authorize() {
        let e = AuthError::NotAuthorized {
            server: "docs".into(),
            reason: "not authorized yet".into(),
        };
        assert!(e.to_string().contains("harness login mcp docs"), "{e}");
        assert!(e.to_string().contains("/login mcp docs"), "{e}");
        assert_eq!(login_command("x"), "harness login mcp x");
    }
}
