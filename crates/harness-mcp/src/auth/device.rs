//! The device authorization grant (RFC 8628), for authorization servers
//! that advertise a `device_authorization_endpoint`. [`super::Authorize`]
//! uses it instead of the loopback redirect when the browser is on another
//! machine ([`super::Authorize::with_device_grant`]).
//!
//! 1. `request`: `POST` the client id, the scopes and the `resource`
//!    (RFC 8707) to the device authorization endpoint. A refusal (an OAuth
//!    error such as `unauthorized_client`) is returned as
//!    `Started::Refused`, and the caller falls back to the loopback
//!    flow.
//! 2. The user opens the verification URI on any device and confirms the
//!    user code ([`DeviceCode`]).
//! 3. `poll`: the token endpoint with
//!    `grant_type=urn:ietf:params:oauth:grant-type:device_code`, waiting
//!    `interval` seconds between requests (5 when the server names none),
//!    5 seconds more after every `slow_down`, and twice as long after a
//!    network error (RFC 8628 section 3.5). `authorization_pending` keeps
//!    polling; `access_denied` and `expired_token` end it, and so does
//!    `expires_in` or the flow's own timeout, whichever comes first.
//!    Dropping the future stops polling at once.
//!
//! PKCE does not apply: there is no authorization code, and the device
//! code only ever travels between harness and the token endpoint. The
//! device code is held in a [`Zeroizing`] string and, like the user code
//! and the tokens, never logged.

use std::time::Duration;

use harness_providers::signin::DeviceCode;
use serde_json::Value;
use zeroize::Zeroizing;

use super::session::{TokenFailure, TokenResponse, token_call};
use super::store::ClientRegistration;
use super::{AuthError, form_encode, secure_endpoint};

/// The device grant's `grant_type`.
pub const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Polling interval when the server names none (RFC 8628 section 3.2).
pub const DEFAULT_INTERVAL: u64 = 5;

/// What `slow_down` adds to the interval (RFC 8628 section 3.5).
pub const SLOW_DOWN: u64 = 5;

/// A device authorization the user has yet to approve.
pub(crate) struct Pending {
    device_code: Zeroizing<String>,
    /// What the user sees.
    pub(crate) shown: DeviceCode,
    /// Seconds between polls.
    interval: u64,
}

/// The answer of the device authorization endpoint.
pub(crate) enum Started {
    /// A code to show.
    Code(Pending),
    /// The server will not do the device grant for this client.
    Refused {
        /// OAuth `error`.
        code: String,
    },
}

/// The client authentication fields of every request.
fn client_fields<'a>(client: &'a ClientRegistration, form: &mut Vec<(&'a str, &'a str)>) {
    form.push(("client_id", client.client_id.as_str()));
    if let Some(s) = &client.client_secret {
        form.push(("client_secret", s.expose()));
    }
}

/// Asks `endpoint` for a device code for `client`.
pub(crate) async fn request(
    http: &reqwest::Client,
    endpoint: &str,
    client: &ClientRegistration,
    scope: &str,
    resource: &str,
) -> Result<Started, AuthError> {
    let mut form = Vec::new();
    client_fields(client, &mut form);
    if !scope.is_empty() {
        form.push(("scope", scope));
    }
    form.push(("resource", resource));
    let resp = http
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(form_encode(&form))
        .send()
        .await
        .map_err(|e| AuthError::Network(e.without_url().to_string()))?;
    let status = resp.status();
    let text = Zeroizing::new(
        resp.text()
            .await
            .map_err(|e| AuthError::Network(e.without_url().to_string()))?,
    );
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !status.is_success() {
        return match v["error"].as_str() {
            Some(code) if status.is_client_error() => Ok(Started::Refused {
                code: code.to_owned(),
            }),
            _ => Err(AuthError::Network(format!(
                "the device authorization endpoint answered HTTP {}",
                status.as_u16()
            ))),
        };
    }
    let field = |k: &str| v[k].as_str().filter(|s| !s.trim().is_empty());
    let unreadable = |what: &str| {
        AuthError::Discovery(format!(
            "the device authorization endpoint sent no usable {what}; set oauth_client_id or try from a machine with a browser"
        ))
    };
    let device_code = field("device_code").ok_or_else(|| unreadable("device_code"))?;
    let user_code = field("user_code").ok_or_else(|| unreadable("user_code"))?;
    // `verification_url` is an older spelling some servers still send.
    let verification_uri = field("verification_uri")
        .or_else(|| field("verification_url"))
        .filter(|u| secure_endpoint(u))
        .ok_or_else(|| unreadable("https:// verification_uri"))?;
    let complete = field("verification_uri_complete").filter(|u| secure_endpoint(u));
    Ok(Started::Code(Pending {
        device_code: Zeroizing::new(device_code.to_owned()),
        shown: DeviceCode {
            user_code: Zeroizing::new(user_code.to_owned()),
            verification_uri: verification_uri.to_owned(),
            verification_uri_complete: complete.map(|c| Zeroizing::new(c.to_owned())),
            expires_in: v["expires_in"].as_u64().filter(|s| *s > 0).unwrap_or(1800),
        },
        interval: v["interval"]
            .as_u64()
            .unwrap_or(DEFAULT_INTERVAL)
            .clamp(1, 600),
    }))
}

/// Polls `token_endpoint` until the user approves `pending` (the tokens),
/// declines it, or it expires. `unit` is the length of one of the
/// server's seconds (tests shorten it); `timeout` caps the whole wait.
pub(crate) async fn poll(
    http: &reqwest::Client,
    token_endpoint: &str,
    client: &ClientRegistration,
    resource: &str,
    pending: &Pending,
    unit: Duration,
    timeout: Duration,
) -> Result<TokenResponse, AuthError> {
    let lifetime = unit.saturating_mul(u32::try_from(pending.shown.expires_in).unwrap_or(u32::MAX));
    let mut network: Option<String> = None;
    let polling = async {
        let mut interval = pending.interval;
        loop {
            tokio::time::sleep(unit.saturating_mul(u32::try_from(interval).unwrap_or(u32::MAX)))
                .await;
            let mut form = vec![
                ("grant_type", DEVICE_GRANT),
                ("device_code", pending.device_code.as_str()),
            ];
            client_fields(client, &mut form);
            form.push(("resource", resource));
            match token_call(http, token_endpoint, &form).await {
                Ok(t) => return Ok(t),
                Err(TokenFailure::OAuth { code, message }) => match code.as_str() {
                    "authorization_pending" => {}
                    "slow_down" => interval += SLOW_DOWN,
                    "access_denied" => return Err(AuthError::Denied),
                    "expired_token" => return Err(AuthError::DeviceExpired),
                    _ => return Err(AuthError::OAuth { code, message }),
                },
                Err(TokenFailure::Network(m)) => {
                    tracing::debug!(error = %m, "device authorization poll failed; backing off");
                    network = Some(m);
                    interval = (interval * 2).min(600);
                }
            }
        }
    };
    let done = tokio::time::timeout(lifetime.min(timeout), polling).await;
    match done {
        Ok(r) => r,
        Err(_) => Err(match network {
            Some(m) => AuthError::Network(m),
            None if lifetime <= timeout => AuthError::DeviceExpired,
            None => AuthError::Timeout(timeout),
        }),
    }
}
