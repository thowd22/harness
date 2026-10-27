//! TASK-162: browser sign-ins from the TUI when the browser is on another
//! machine. Sign in with ChatGPT and MCP OAuth run in the background, the
//! sign-in modal gets the link (and the `ssh -L` command over SSH), pasted
//! addresses go to the flow and are answered, and cancelling stops the
//! flow and closes its loopback listener. Codes and tokens never reach the
//! UI.

use harness_config::ini::edit::Credential;
use harness_config::login::LoginRequest;
use harness_providers::chatgpt::Browser;
use harness_providers::chatgpt::mock::{MockBrowser, MockConfig};
use harness_providers::signin::{Environment, redirect_port};
use harness_tui::login::LoginSubmission;
use harness_tui::signin::{CHATGPT_ID, PastedText, SignInPrompt};

use super::*;
use crate::login::chatgpt_tests::MockSignIn;

fn over_ssh() -> Environment {
    Environment::from_env(&|k| match k {
        "SSH_CONNECTION" => Some("10.0.0.7 50122 10.0.0.2 22".into()),
        "USER" => Some("dev".into()),
        _ => None,
    })
}

async fn chatgpt_harness(env: &Env) -> (Harness, PathBuf, Arc<MockSignIn>) {
    let script = env.script(TEXT);
    let mut h = Harness::with_ini(env, &script, false, "").await;
    let ini = env.root.join("cfg").join("harness.ini");
    std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
    std::fs::write(
        &ini,
        format!(
            "[general]\ndata_dir = {}\n",
            env.root.join("data").display()
        ),
    )
    .unwrap();
    h.d.login_path = ini.clone();
    let v = Arc::new(MockSignIn::start(MockConfig::default()).await);
    h.d.login_verifier = v.clone();
    h.d.sign_ins.env = over_ssh();
    (h, ini, v)
}

fn chatgpt() -> UiRequest {
    UiRequest::Login(LoginSubmission::new(LoginRequest {
        provider: "openai",
        credential: Credential::ChatGpt,
    }))
}

async fn prompt(h: &mut Harness, id: &str) -> SignInPrompt {
    match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::SignIn(p)) if p.id == id))
        .await
    {
        AppEvent::Core(CoreEvent::SignIn(p)) => p.clone(),
        _ => unreachable!(),
    }
}

async fn answer(h: &mut Harness, id: &str) -> Result<(), String> {
    let at = h.seen.len();
    let id = id.to_owned();
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::SignInAnswer { .. })))
        .await;
    let i = h.seen[at..]
        .iter()
        .position(
            |e| matches!(e, AppEvent::Core(CoreEvent::SignInAnswer { id: i, .. }) if *i == id),
        )
        .expect("an answer for this sign-in");
    match h.seen.remove(at + i) {
        AppEvent::Core(CoreEvent::SignInAnswer { result, .. }) => result,
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn chatgpt_over_ssh_shows_the_link_and_takes_the_pasted_address() {
    let env = Env::new();
    let (mut h, ini, v) = chatgpt_harness(&env).await;
    // The driver does not wait for the browser: the request returns at once.
    tokio::time::timeout(Duration::from_secs(2), h.d.request(chatgpt()))
        .await
        .expect("the sign-in runs in the background");
    let p = prompt(&mut h, CHATGPT_ID).await;
    assert_eq!(p.title, "Sign in with ChatGPT");
    assert_eq!(p.no_browser.as_deref(), Some("this is an SSH session"));
    let port = redirect_port(&p.url).unwrap();
    assert_eq!(
        p.forward.as_deref(),
        Some(format!("ssh -N -L {port}:127.0.0.1:{port} dev@10.0.0.2").as_str())
    );
    assert!(v.browser.opened().is_empty(), "no local browser over SSH");

    // The user's own machine signs in and lands on the loopback address.
    let elsewhere = MockBrowser::remote();
    elsewhere.open(&p.url).unwrap();
    let address = elsewhere.next_redirect().await;
    let paste = |t: &str| UiRequest::SignInPaste {
        id: CHATGPT_ID.into(),
        text: PastedText::new(t.to_owned()),
    };
    h.d.request(paste("nonsense here")).await;
    assert!(answer(&mut h, CHATGPT_ID).await.is_err());
    h.d.request(paste(&address)).await;
    assert_eq!(answer(&mut h, CHATGPT_ID).await, Ok(()));
    match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::LoginResult(_))))
        .await
    {
        AppEvent::Core(CoreEvent::LoginResult(r)) => {
            assert!(
                r.as_ref().unwrap().contains("signed in with ChatGPT"),
                "{r:?}"
            )
        }
        _ => unreachable!(),
    }
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::SignInDone { id, result: Ok(_) }) if id == CHATGPT_ID))
        .await;
    assert!(
        std::fs::read_to_string(&ini)
            .unwrap()
            .contains("auth = chatgpt")
    );
    // The provider config was picked up for the next session.
    assert!(crate::wiring::openai_uses_chatgpt(&h.d.cfg));

    let code = address
        .split('&')
        .find_map(|p| p.split("code=").nth(1))
        .unwrap()
        .to_owned();
    let seen = format!("{:?}", h.seen);
    assert!(!seen.contains(&code), "the code reached the UI");
    let cfg = harness_config::ini::load_file(&ini).unwrap();
    let tokens = crate::wiring::chatgpt_store(&cfg)
        .load()
        .unwrap()
        .tokens
        .unwrap();
    assert!(!seen.contains(tokens.access_token.expose()));
}

#[tokio::test]
async fn cancelling_a_sign_in_stops_it_and_closes_the_listener() {
    let env = Env::new();
    let (mut h, ini, _) = chatgpt_harness(&env).await;
    h.d.request(chatgpt()).await;
    let p = prompt(&mut h, CHATGPT_ID).await;
    let port = redirect_port(&p.url).unwrap();
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok(),
        "listening while it runs"
    );
    h.d.request(UiRequest::SignInCancel(CHATGPT_ID.into()))
        .await;
    let mut closed = false;
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(closed, "the loopback listener closed");
    // A paste after the cancel is answered, not lost.
    h.d.request(UiRequest::SignInPaste {
        id: CHATGPT_ID.into(),
        text: PastedText::new("?code=late&state=x".into()),
    })
    .await;
    let e = answer(&mut h, CHATGPT_ID).await.unwrap_err();
    assert!(e.contains("no longer running"), "{e}");
    assert!(!std::fs::read_to_string(&ini).unwrap().contains("auth ="));
    assert!(
        !h.seen
            .iter()
            .any(|e| matches!(e, AppEvent::Core(CoreEvent::LoginResult(_)))),
        "a cancelled sign-in reports nothing"
    );
}

#[tokio::test]
async fn mcp_over_ssh_takes_the_pasted_address() {
    use harness_mcp::auth::mock::{MockBrowser, MockConfig, MockOAuthServer};
    let mock = MockOAuthServer::start(MockConfig::default()).await;
    let env = Env::new();
    let script = env.script(TEXT);
    let ini_text = format!(
        "[general]\ndata_dir = {}\n\n[mcp.docs]\nurl = {}\n",
        env.root.join("data").display(),
        mock.mcp_url()
    );
    let mut h = Harness::with_ini(&env, &script, false, &ini_text).await;
    let ini = env.root.join("cfg").join("harness.ini");
    std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
    std::fs::write(&ini, &ini_text).unwrap();
    h.d.login_path = ini.clone();
    let local = Arc::new(MockBrowser::new());
    h.d.browser = local.clone();
    h.d.sign_ins.env = over_ssh();

    h.d.request(UiRequest::Submit("/login mcp docs".into()))
        .await;
    let p = prompt(&mut h, "mcp.docs").await;
    assert_eq!(p.no_browser.as_deref(), Some("this is an SSH session"));
    assert!(p.forward.as_deref().unwrap().starts_with("ssh -N -L "));
    assert!(local.opened().is_empty());
    let elsewhere = MockBrowser::remote();
    elsewhere.open(&p.url).unwrap();
    let address = elsewhere.next_redirect().await;
    h.d.request(UiRequest::SignInPaste {
        id: "mcp.docs".into(),
        text: PastedText::new(address),
    })
    .await;
    assert_eq!(answer(&mut h, "mcp.docs").await, Ok(()));
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "MCP server docs authorized"))
        .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::SignInDone { id, result: Ok(_) }) if id == "mcp.docs"))
        .await;
    assert!(
        std::fs::read_to_string(&ini)
            .unwrap()
            .contains("auth = oauth")
    );
}

/// TASK-165: a TUI harness with `[mcp.docs]` on a mock that offers the
/// device grant, over SSH.
async fn device_harness(
    env: &Env,
) -> (
    Harness,
    PathBuf,
    harness_mcp::auth::mock::MockOAuthServer,
    Arc<harness_mcp::auth::mock::MockBrowser>,
) {
    use harness_mcp::auth::mock::{MockBrowser, MockConfig, MockOAuthServer};
    let mock = MockOAuthServer::start(MockConfig {
        device_authorization: true,
        ..MockConfig::default()
    })
    .await;
    let script = env.script(TEXT);
    let ini_text = format!(
        "[general]\ndata_dir = {}\n\n[mcp.docs]\nurl = {}\n",
        env.root.join("data").display(),
        mock.mcp_url()
    );
    let mut h = Harness::with_ini(env, &script, false, &ini_text).await;
    let ini = env.root.join("cfg").join("harness.ini");
    std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
    std::fs::write(&ini, &ini_text).unwrap();
    h.d.login_path = ini.clone();
    let local = Arc::new(MockBrowser::new());
    h.d.browser = local.clone();
    h.d.sign_ins.env = over_ssh();
    (h, ini, mock, local)
}

#[tokio::test]
async fn mcp_over_ssh_shows_a_device_code_and_approving_it_authorizes() {
    use harness_mcp::auth::mock::MockBrowser;
    let env = Env::new();
    let (mut h, ini, mock, local) = device_harness(&env).await;
    h.d.request(UiRequest::Submit("/login mcp docs".into()))
        .await;
    let p = prompt(&mut h, "mcp.docs").await;
    let device = p.device.clone().expect("a device code");
    assert_eq!(p.url, device.link());
    assert!(p.url.contains(device.user_code.as_str()));
    assert_eq!(p.no_browser.as_deref(), Some("this is an SSH session"));
    assert_eq!(p.forward, None, "nothing comes back to forward");
    assert!(local.opened().is_empty(), "no local browser over SSH");
    assert!(mock.requests_to("/authorize").is_empty());

    // The user approves on their own machine; harness finishes by itself.
    MockBrowser::remote().open(&p.url).unwrap();
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "MCP server docs authorized"))
        .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::SignInDone { id, result: Ok(_) }) if id == "mcp.docs"))
        .await;
    assert!(
        std::fs::read_to_string(&ini)
            .unwrap()
            .contains("auth = oauth")
    );
    let cfg = harness_config::ini::load_file(&ini).unwrap();
    let store = crate::wiring::mcp_token_store(&cfg);
    assert!(store.has_tokens("docs"));
    // The device code and the tokens never reach the UI events; their
    // Debug form hides the user code too.
    let device_code = mock.requests_to("/token")[0].param("device_code").unwrap();
    let seen = format!("{:?}", h.seen);
    assert!(
        !seen.contains(&device_code),
        "the device code reached the UI"
    );
    assert!(!seen.contains(device.user_code.as_str()));
    for t in store.secrets() {
        assert!(!seen.contains(t.as_str()), "a token reached the UI");
    }
}

#[tokio::test]
async fn cancelling_a_device_sign_in_stops_polling() {
    use harness_mcp::auth::mock::MockBrowser;
    let env = Env::new();
    let (mut h, ini, mock, _) = device_harness(&env).await;
    h.d.request(UiRequest::Submit("/login mcp docs".into()))
        .await;
    let p = prompt(&mut h, "mcp.docs").await;
    assert!(p.device.is_some());
    // Esc in the modal sends this; the approval comes too late.
    h.d.request(UiRequest::SignInCancel("mcp.docs".into()))
        .await;
    MockBrowser::remote().open(&p.url).unwrap();
    // The mock's interval is 1 s: a running flow would have polled.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(mock.requests_to("/token").is_empty(), "polling stopped");
    assert!(!std::fs::read_to_string(&ini).unwrap().contains("auth ="));
    assert!(
        !h.seen.iter().any(
            |e| matches!(e, AppEvent::Core(CoreEvent::SignInDone { id, .. }) if id == "mcp.docs")
        ),
        "a cancelled sign-in reports nothing"
    );
}
