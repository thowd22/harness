//! Sign in with ChatGPT with the browser on another machine: the user
//! pastes the redirected address (or the code) back, or forwards the
//! loopback port the way `ssh -L` does. Runs against the local mock
//! authorization server; no real account is used.

use std::time::Duration;

use harness_providers::chatgpt::mock::{MockAuthServer, MockBrowser, MockConfig};
use harness_providers::chatgpt::store::{AuthRecord, TokenStore};
use harness_providers::chatgpt::{ChatGptError, SignIn};
use harness_providers::signin::{
    Environment, Instructions, Paste, PasteSender, paste_channel, redirect_port,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn sign_in(server: &MockAuthServer, store: &TokenStore) -> SignIn {
    SignIn::new(store.clone())
        .with_endpoints(server.endpoints())
        .with_port(0)
        .with_timeout(Duration::from_secs(10))
}

fn store() -> (tempfile::TempDir, TokenStore) {
    let tmp = tempfile::tempdir().unwrap();
    let store = TokenStore::new(tmp.path().join("auth"));
    (tmp, store)
}

/// Pastes `text` and returns the flow's answer.
async fn paste(tx: &PasteSender, text: &str) -> Result<(), String> {
    let (p, reply) = Paste::new(text);
    tx.send(p).unwrap();
    reply.await.unwrap()
}

#[tokio::test]
async fn pasting_the_redirected_address_completes_the_first_sign_in() {
    let server = MockAuthServer::start(MockConfig::default()).await;
    let (_tmp, store) = store();
    let browser = MockBrowser::remote();
    let (tx, mut rx) = paste_channel();
    let flow = sign_in(&server, &store);
    let user = async {
        let address = browser.next_redirect().await;
        assert!(address.starts_with("http://127.0.0.1:"), "{address}");
        // Mistakes are answered and the flow keeps waiting.
        assert!(paste(&tx, "hello there").await.is_err());
        let code_only = address
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let e = paste(&tx, code_only).await.unwrap_err();
        assert!(e.contains("whole address"), "{e}");
        let stale = address.replace("state=", "state=x");
        let e = paste(&tx, &stale).await.unwrap_err();
        assert!(e.contains("another sign-in attempt"), "{e}");
        assert!(server.requests_to("/api/accounts/oauth/token").is_empty());
        assert_eq!(paste(&tx, &format!("  {address}\n")).await, Ok(()));
    };
    let (account, ()) = tokio::join!(flow.run_with(&browser, &mut rx), user);
    let account = account.unwrap();
    assert_eq!(account.email.as_deref(), Some("user@example.com"));

    // Same exchange as a real callback: issued client id, PKCE verifier and
    // the loopback redirect_uri.
    let auth = &server.requests_to("/api/accounts/authorize")[0];
    let token = &server.requests_to("/api/accounts/oauth/token")[0];
    assert_eq!(token.form("client_id").as_deref(), Some("oaiapp_mock123"));
    assert!(token.form("code_verifier").is_some());
    assert_eq!(
        token.form("redirect_uri").as_deref(),
        auth.param("redirect_uri")
    );
    let record = store.load().unwrap();
    assert_eq!(record.client_id.as_deref(), Some("oaiapp_mock123"));
    assert!(record.tokens.unwrap().plan_usage());
}

#[tokio::test]
async fn a_bare_code_or_query_works_once_registered() {
    let server = MockAuthServer::start(MockConfig::default()).await;
    let (_tmp, store) = store();
    sign_in(&server, &store)
        .run(&MockBrowser::new())
        .await
        .unwrap();

    for form in ["code", "query"] {
        let browser = MockBrowser::remote();
        let (tx, mut rx) = paste_channel();
        let flow = sign_in(&server, &store);
        let user = async {
            let address = browser.next_redirect().await;
            let query = address.split_once('?').unwrap().1.to_owned();
            let text = if form == "code" {
                query
                    .split('&')
                    .find_map(|p| p.strip_prefix("code="))
                    .unwrap()
                    .to_owned()
            } else {
                format!("?{query}")
            };
            assert_eq!(paste(&tx, &text).await, Ok(()), "{form}");
        };
        let (r, ()) = tokio::join!(flow.run_with(&browser, &mut rx), user);
        r.unwrap();
    }
    assert_eq!(server.requests_to("/api/accounts/oauth/token").len(), 3);
}

#[tokio::test]
async fn a_pasted_refusal_saves_nothing() {
    let server = MockAuthServer::start(MockConfig {
        deny: true,
        ..MockConfig::default()
    })
    .await;
    let (_tmp, store) = store();
    let browser = MockBrowser::remote();
    let (tx, mut rx) = paste_channel();
    let flow = sign_in(&server, &store);
    let user = async {
        let address = browser.next_redirect().await;
        assert_eq!(paste(&tx, &address).await, Ok(()));
    };
    let (r, ()) = tokio::join!(flow.run_with(&browser, &mut rx), user);
    assert_eq!(r.unwrap_err(), ChatGptError::Denied);
    assert_eq!(store.load().unwrap(), AuthRecord::default());
}

#[tokio::test]
async fn an_abandoned_remote_sign_in_times_out() {
    let server = MockAuthServer::start(MockConfig::default()).await;
    let (_tmp, store) = store();
    let (_tx, mut rx) = paste_channel();
    let err = sign_in(&server, &store)
        .with_timeout(Duration::from_millis(200))
        .run_with(&MockBrowser::remote(), &mut rx)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatGptError::Timeout(_)), "{err:?}");
    assert!(store.load().unwrap().tokens.is_none());
}

/// Forwards every connection on `from` to `to`, as `ssh -L` does.
async fn forward(from: TcpListener, to: u16) {
    loop {
        let Ok((mut inbound, _)) = from.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let Ok(mut outbound) = tokio::net::TcpStream::connect(("127.0.0.1", to)).await else {
                return;
            };
            let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
        });
    }
}

#[tokio::test]
async fn the_port_forward_carries_the_browser_back() {
    let server = MockAuthServer::start(MockConfig::default()).await;
    let (_tmp, store) = store();
    let browser = MockBrowser::remote();
    let (_tx, mut rx) = paste_channel();
    let flow = sign_in(&server, &store);
    let env = Environment::from_env(&|k| match k {
        "SSH_CONNECTION" => Some("10.0.0.9 50000 10.0.0.1 22".into()),
        "USER" => Some("dev".into()),
        _ => None,
    });
    let user = async {
        let address = browser.next_redirect().await;
        let url = browser.opened()[0].clone();
        let port = redirect_port(&url).unwrap();
        let shown = Instructions::new(&url, &env);
        assert_eq!(
            shown.forward.as_deref(),
            Some(format!("ssh -N -L {port}:127.0.0.1:{port} dev@10.0.0.1").as_str())
        );
        // The browser machine's end of the tunnel is another port here,
        // since both ends share one machine in the test.
        let tunnel = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = tunnel.local_addr().unwrap().port();
        tokio::spawn(forward(tunnel, port));
        let through = address.replace(&format!(":{port}/"), &format!(":{local}/"));
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", local))
            .await
            .unwrap();
        let target = through.split_once(&format!(":{local}")).unwrap().1;
        sock.write_all(format!("GET {target} HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut page = String::new();
        sock.read_to_string(&mut page).await.unwrap();
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert!(page.contains("Signed in to harness"));
    };
    let (r, ()) = tokio::join!(flow.run_with(&browser, &mut rx), user);
    r.unwrap();
    assert!(store.load().unwrap().tokens.is_some());
}

/// Sends a GET for `address` straight to the loopback port and returns the
/// whole response.
async fn get(address: &str) -> String {
    let rest = address.strip_prefix("http://127.0.0.1:").unwrap();
    let (port, target) = rest.split_at(rest.find('/').unwrap());
    let mut sock = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    sock.write_all(format!("GET {target} HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut page = String::new();
    sock.read_to_string(&mut page).await.unwrap();
    page
}

#[tokio::test]
async fn a_stale_callback_is_refused_and_the_sign_in_keeps_waiting() {
    let server = MockAuthServer::start(MockConfig::default()).await;
    let (_tmp, store) = store();
    let browser = MockBrowser::remote();
    let (_tx, mut rx) = paste_channel();
    let flow = sign_in(&server, &store);
    let user = async {
        let address = browser.next_redirect().await;
        let page = get(&address.replace("state=", "state=old")).await;
        assert!(page.starts_with("HTTP/1.1 400"), "{page}");
        assert!(page.contains("stale"), "{page}");
        assert!(server.requests_to("/api/accounts/oauth/token").is_empty());
        let page = get(&address).await;
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    };
    let (r, ()) = tokio::join!(flow.run_with(&browser, &mut rx), user);
    r.unwrap();
    assert!(store.load().unwrap().tokens.is_some());
}
