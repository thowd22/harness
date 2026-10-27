//! `harness login` / `harness logout`, and [`apply`]: the verify-then-save
//! step shared with the TUI `/login` action.
//!
//! The TUI drives a [`harness_config::login::LoginFlow`]; when it returns a
//! [`LoginRequest`], the TUI calls [`apply`] with a [`LiveVerifier`] and
//! passes the result to [`LoginFlow::finish`](harness_config::login::LoginFlow::finish).
//! The key is held in a [`Secret`] throughout and never logged, printed or
//! put in an event.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use harness_config::ini::document::parse;
use harness_config::ini::edit::{self, Credential};
use harness_config::login::{AUTH_SUPPORT, LoginRequest, MASK_CHAR, Method, auth_support};
use harness_config::paths::config_location;
use harness_config::secrets::{DEFAULT_CMD_TIMEOUT, Secret, run_secret_command};
use harness_providers::chatgpt::{Account, Browser, ChatGptError, Endpoints, SignIn, TokenStore};
use harness_providers::login::{KeyCheck, VERIFY_TIMEOUT};
use harness_providers::provider::ProviderError;
use harness_providers::signin::{
    Environment, Instructions, Paste, PasteReceiver, PasteSender, hyperlink, paste_channel,
};
use tokio::sync::mpsc::UnboundedReceiver;

pub mod mcp;

/// Usage of `harness login` and `harness logout`.
pub const USAGE: &str = "\
usage: harness login <anthropic|openai|openrouter> [--api-key-cmd CMD | --api-key-env VAR]
       harness login openai --chatgpt
       harness logout <anthropic|openai|openrouter> [--yes]
       harness login mcp <name>       (OAuth for a remote MCP server; see login mcp --help)
       harness logout mcp <name> [--yes]

login asks for an API key with masked input (or reads one line from stdin
when it is not a terminal), checks it with one cheap authenticated request,
and saves it to [provider.<id>] in harness.ini, which is set to mode 0600.
--api-key-cmd stores a command that prints the key (a password manager);
--api-key-env stores the name of an environment variable. Either is checked
the same way before it is saved. --chatgpt (or choosing Sign in with
ChatGPT in the menu) opens the browser to sign in with your ChatGPT plan,
stores the tokens in <data_dir>/auth (mode 0600, never in harness.ini) and
sets auth = chatgpt in [provider.openai]. logout removes api_key,
api_key_cmd, api_key_env and auth from the section after confirmation
(--yes skips it); for openai it also deletes the ChatGPT tokens.

Supported auth:
  anthropic   API key only (Claude subscription login is not supported)
  openai      API key, or Sign in with ChatGPT
  openrouter  API key";

/// Checks a key with the provider.
#[async_trait]
pub trait Verifier: Send + Sync {
    /// One cheap authenticated request; [`ProviderError::Auth`] for a
    /// rejected key.
    async fn verify(
        &self,
        provider: &str,
        key: &Secret,
        base_url: Option<&str>,
    ) -> Result<KeyCheck, ProviderError>;

    /// The browser Sign in with ChatGPT opens (the system browser).
    fn browser(&self) -> &dyn Browser {
        &harness_providers::chatgpt::SystemBrowser
    }

    /// Runs Sign in with ChatGPT in `browser`, storing tokens in `store`;
    /// a callback pasted into `paste` finishes it as well.
    async fn sign_in_chatgpt(
        &self,
        store: &TokenStore,
        endpoints: &Endpoints,
        browser: &dyn Browser,
        paste: &mut PasteReceiver,
    ) -> Result<Account, ChatGptError> {
        SignIn::new(store.clone())
            .with_endpoints(endpoints.clone())
            .run_with(browser, paste)
            .await
    }
}

/// The real [`Verifier`]: [`verify_api_key`](harness_providers::login::verify_api_key) over HTTPS.
#[derive(Debug, Default, Clone)]
pub struct LiveVerifier {
    client: harness_providers::login::KeyVerifier,
}

#[async_trait]
impl Verifier for LiveVerifier {
    async fn verify(
        &self,
        provider: &str,
        key: &Secret,
        base_url: Option<&str>,
    ) -> Result<KeyCheck, ProviderError> {
        self.client
            .verify(provider, key.expose(), base_url, VERIFY_TIMEOUT)
            .await
    }
}

/// The `harness.ini` that login and logout edit: `$HARNESS_CONFIG`, else
/// the XDG default (created when missing).
pub fn config_path() -> PathBuf {
    config_location(|k| std::env::var_os(k))
        .path()
        .to_path_buf()
}

/// The `base_url` configured for `provider` in the file, if any.
fn base_url(path: &Path, provider: &str) -> Result<Option<String>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let doc = parse(&text, Some(path)).map_err(|_| {
        format!(
            "{}: cannot edit a file with syntax errors; run `harness config check` and fix them first",
            path.display()
        )
    })?;
    Ok(doc
        .get(&format!("provider.{provider}"), "base_url")
        .map(|e| e.value.clone())
        .filter(|v| !v.is_empty()))
}

/// The key a credential yields now, for verification.
fn resolve(
    credential: &Credential,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Result<Secret, String> {
    match credential {
        Credential::ApiKey(k) => Ok(k.clone()),
        Credential::Command(cmd) => {
            let key = run_secret_command(cmd, DEFAULT_CMD_TIMEOUT).map_err(|e| e.to_string())?;
            if key.is_empty() {
                Err("api_key_cmd printed nothing; check the command".into())
            } else {
                Ok(key)
            }
        }
        Credential::ChatGpt => Err("Sign in with ChatGPT has no key to check".into()),
        Credential::Env(var) => env(var)
            .filter(|v| !v.trim().is_empty())
            .map(|v| Secret::new(v.trim().to_owned()))
            .ok_or_else(|| {
                format!(
                    "{var} is not set in this shell; export it first, or store the key another way"
                )
            }),
    }
}

/// Explains a failed check. Never contains the key.
fn failure(provider: &str, base: Option<&str>, err: &ProviderError) -> String {
    let support = auth_support(provider);
    let label = support.map_or(provider, |s| s.label);
    let url = harness_providers::login::verify_url(provider, base).unwrap_or_default();
    match err {
        ProviderError::Auth(m) => format!(
            "{label} rejected the key ({m}). Check that you copied the whole key{}. Nothing was saved.",
            support.map_or(String::new(), |s| format!(
                ", or create a new one at {}",
                s.key_url
            ))
        ),
        ProviderError::Connection(m) => format!(
            "could not reach {url} to check the key ({m}). Check your network or base_url in [provider.{provider}]. Nothing was saved."
        ),
        ProviderError::Timeout(d) => format!(
            "{url} did not answer within {}s. Check your network or base_url in [provider.{provider}]. Nothing was saved.",
            d.as_secs()
        ),
        other => format!("checking the key with {label} failed: {other}. Nothing was saved."),
    }
}

/// Verifies `request` and, only if the provider accepts the key, stores it
/// in the file at `path` (mode `0600`). Returns a one-line summary or an
/// actionable error; neither contains the key.
pub async fn apply(
    path: &Path,
    request: &LoginRequest,
    verifier: &dyn Verifier,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Result<String, String> {
    let (_keep, mut paste) = paste_channel();
    apply_with(path, request, verifier, env, verifier.browser(), &mut paste).await
}

/// The config in the file at `path` (defaults when it does not exist).
fn config_at(path: &Path) -> Result<harness_config::ini::Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => harness_config::ini::load_str(&text, Some(path))
            .map_err(|e| format!("{}:\n{e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(harness_config::ini::Config::defaults())
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Sign in with ChatGPT for `[provider.openai]` in the file at `path`:
/// browser sign-in, tokens to the protected token store, then
/// `auth = chatgpt`.
async fn apply_chatgpt(
    path: &Path,
    verifier: &dyn Verifier,
    browser: &dyn Browser,
    paste: &mut PasteReceiver,
) -> Result<String, String> {
    let cfg = config_at(path)?;
    let store = crate::wiring::chatgpt_store(&cfg);
    let account = verifier
        .sign_in_chatgpt(
            &store,
            &crate::wiring::chatgpt_endpoints(&cfg),
            browser,
            paste,
        )
        .await
        .map_err(|e| e.to_string())?;
    edit::save_credential(path, "openai", &Credential::ChatGpt).map_err(|e| e.to_string())?;
    Ok(format!(
        "signed in with ChatGPT as {} with plan usage; set auth = chatgpt in {} (tokens are in {}, mode 0600)",
        account.label(),
        path.display(),
        store.dir().display()
    ))
}

/// [`apply`] with the browser that Sign in with ChatGPT opens and the
/// callbacks the user pastes.
pub(crate) async fn apply_with(
    path: &Path,
    request: &LoginRequest,
    verifier: &dyn Verifier,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    browser: &dyn Browser,
    paste: &mut PasteReceiver,
) -> Result<String, String> {
    let provider = request.provider;
    if matches!(request.credential, Credential::ChatGpt) {
        if provider != harness_providers::openai::PROVIDER_NAME {
            return Err(format!(
                "Sign in with ChatGPT is only for openai, not {provider}"
            ));
        }
        return apply_chatgpt(path, verifier, browser, paste).await;
    }
    let label = auth_support(provider).map_or(provider, |s| s.label);
    request.credential.validate().map_err(|e| e.to_string())?;
    let base = base_url(path, provider)?;
    let key = resolve(&request.credential, env)?;
    let check = verifier
        .verify(provider, &key, base.as_deref())
        .await
        .map_err(|e| failure(provider, base.as_deref(), &e))?;
    edit::save_credential(path, provider, &request.credential).map_err(|e| e.to_string())?;
    let detail = check.detail.map(|d| format!(" ({d})")).unwrap_or_default();
    Ok(format!(
        "{label} accepted the key{detail}; saved {} to {} (mode 0600)",
        request.credential.ini_key(),
        path.display()
    ))
}

/// Reads input for `harness login` / `logout`.
pub trait Prompt {
    /// True when the user can answer questions (stdin is a terminal).
    fn interactive(&self) -> bool;
    /// Reads a secret without echoing it. `None` when cancelled or at EOF.
    fn secret(&mut self, prompt: &str) -> std::io::Result<Option<Secret>>;
    /// Reads one visible line. `None` at EOF.
    fn line(&mut self, prompt: &str) -> std::io::Result<Option<String>>;
    /// Whether links may be printed as OSC 8 hyperlinks (stderr is a
    /// terminal).
    fn hyperlinks(&self) -> bool {
        false
    }
    /// Lines typed while a browser sign-in waits (pasted callbacks), or
    /// `None` when nobody can type.
    fn paste_lines(&mut self) -> Option<UnboundedReceiver<String>> {
        None
    }
}

/// [`Prompt`] on the process terminal: masked raw-mode input on a tty,
/// one line from stdin otherwise. Prompts go to stderr.
#[derive(Debug, Default)]
pub struct TerminalPrompt;

impl Prompt for TerminalPrompt {
    fn interactive(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn secret(&mut self, prompt: &str) -> std::io::Result<Option<Secret>> {
        if !self.interactive() {
            return read_line_secret();
        }
        let mut err = std::io::stderr();
        write!(err, "{prompt}")?;
        err.flush()?;
        crossterm::terminal::enable_raw_mode()?;
        let result = read_masked(&mut err);
        let _ = crossterm::terminal::disable_raw_mode();
        writeln!(err)?;
        result
    }

    fn line(&mut self, prompt: &str) -> std::io::Result<Option<String>> {
        let mut err = std::io::stderr();
        write!(err, "{prompt}")?;
        err.flush()?;
        let mut s = String::new();
        if std::io::stdin().lock().read_line(&mut s)? == 0 {
            return Ok(None);
        }
        Ok(Some(s.trim().to_owned()))
    }

    fn hyperlinks(&self) -> bool {
        std::io::stderr().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb")
    }

    fn paste_lines(&mut self) -> Option<UnboundedReceiver<String>> {
        if !self.interactive() {
            return None;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        // Blocks on stdin until a line comes or the process exits.
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Some(rx)
    }
}

fn read_line_secret() -> std::io::Result<Option<Secret>> {
    let mut s = zeroize::Zeroizing::new(String::new());
    if std::io::stdin().lock().read_line(&mut s)? == 0 {
        return Ok(None);
    }
    Ok(Some(Secret::new(s.trim().to_owned())))
}

/// Reads keys in raw mode until Enter, showing one mask character per
/// typed character. Esc and Ctrl+C cancel.
fn read_masked(err: &mut dyn Write) -> std::io::Result<Option<Secret>> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    let mut input = harness_config::login::MaskedInput::new();
    loop {
        match read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => return Ok(Some(input.take())),
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c' | 'd') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None);
                }
                KeyCode::Backspace => {
                    if !input.is_empty() {
                        input.backspace();
                        write!(err, "\x08 \x08")?;
                    }
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    write!(err, "{MASK_CHAR}")?;
                }
                _ => {}
            },
            Event::Paste(text) => {
                let before = input.len();
                input.push_str(&text);
                for _ in before..input.len() {
                    write!(err, "{MASK_CHAR}")?;
                }
            }
            _ => {}
        }
        err.flush()?;
    }
}

fn known_provider(p: Option<&String>) -> Result<&'static str, String> {
    let Some(p) = p else {
        return Err("a provider is required".into());
    };
    AUTH_SUPPORT
        .iter()
        .find(|a| a.provider == p)
        .map(|a| a.provider)
        .ok_or_else(|| format!("unknown provider `{p}` (expected anthropic, openai or openrouter)"))
}

/// Parsed `harness login` arguments.
#[derive(Debug, PartialEq, Eq)]
struct LoginArgs {
    provider: &'static str,
    cmd: Option<String>,
    env: Option<String>,
    chatgpt: bool,
}

fn parse_login(args: &[String]) -> Result<LoginArgs, String> {
    let mut provider = None;
    let (mut cmd, mut env) = (None, None);
    let mut chatgpt = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (a.as_str(), None),
        };
        let mut take = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--api-key-cmd" => cmd = Some(take()?),
            "--api-key-env" => env = Some(take()?),
            "--chatgpt" => chatgpt = true,
            f if f.starts_with('-') => return Err(format!("unknown option `{f}`")),
            _ if provider.is_none() => provider = Some(a),
            _ => return Err(format!("unexpected argument `{a}`")),
        }
    }
    if cmd.is_some() && env.is_some() {
        return Err("--api-key-cmd and --api-key-env cannot be combined".into());
    }
    if chatgpt && (cmd.is_some() || env.is_some()) {
        return Err("--chatgpt cannot be combined with --api-key-cmd or --api-key-env".into());
    }
    let provider = known_provider(provider)?;
    if chatgpt && provider != "openai" {
        return Err(format!("--chatgpt is only for openai, not {provider}"));
    }
    Ok(LoginArgs {
        provider,
        cmd,
        env,
        chatgpt,
    })
}

/// Asks which method to use on a terminal; the default is a pasted key.
/// OpenAI also offers Sign in with ChatGPT.
fn choose_method(
    provider: &str,
    prompt: &mut dyn Prompt,
    err: &mut dyn Write,
) -> std::io::Result<Option<Method>> {
    let methods = Method::for_provider(provider);
    writeln!(err, "How should harness get the key?")?;
    for (i, m) in methods.iter().enumerate() {
        writeln!(err, "  {}) {} ({})", i + 1, m.label(), m.detail())?;
    }
    let n = methods.len();
    let hint = if n > 3 {
        "enter 1, 2 or 3, or 4 to sign in with ChatGPT".to_owned()
    } else {
        "enter 1, 2 or 3".to_owned()
    };
    loop {
        let Some(answer) = prompt.line(&format!("Choose 1-{n} [1]: "))? else {
            return Ok(None);
        };
        if answer.is_empty() {
            return Ok(Some(methods[0]));
        }
        match answer.parse::<usize>() {
            Ok(i) if (1..=n).contains(&i) => return Ok(Some(methods[i - 1])),
            _ => writeln!(err, "{hint}")?,
        }
    }
}

/// A [`Browser`] for the terminal: passes the URL on to be printed, then
/// opens the system browser unless the browser is elsewhere.
pub(crate) struct TerminalBrowser<'a> {
    pub(crate) shown: tokio::sync::mpsc::UnboundedSender<String>,
    pub(crate) inner: &'a dyn Browser,
    /// Open `inner` ([`Environment::opens_browser`]).
    pub(crate) open: bool,
}

impl Browser for TerminalBrowser<'_> {
    fn open(&self, url: &str) -> Result<(), String> {
        let _ = self.shown.send(url.to_owned());
        if self.open
            && let Err(e) = self.inner.open(url)
        {
            tracing::warn!(error = %e, "could not open a browser");
        }
        Ok(())
    }
}

/// How a browser sign-in shows itself in the terminal.
pub(crate) struct SignInView<'a> {
    /// What the link is for (`sign in with ChatGPT`).
    pub(crate) what: &'a str,
    /// Where harness runs.
    pub(crate) env: Environment,
    /// Print the link as an OSC 8 hyperlink too.
    pub(crate) links: bool,
}

/// Waits for `running` (a browser sign-in) while printing the link it
/// opens and handing typed lines to it as pasted callbacks. `c` copies the
/// link with OSC 52.
pub(crate) async fn drive_sign_in<T>(
    running: impl std::future::Future<Output = T>,
    mut urls: UnboundedReceiver<String>,
    mut lines: Option<UnboundedReceiver<String>>,
    paste: PasteSender,
    view: &SignInView<'_>,
    env: &dyn Fn(&str) -> Option<String>,
    err: &mut dyn Write,
) -> T {
    tokio::pin!(running);
    let mut url = None;
    // The flow answers pastes in the order they were sent.
    let mut replies: std::collections::VecDeque<
        tokio::sync::oneshot::Receiver<Result<(), String>>,
    > = std::collections::VecDeque::new();
    loop {
        let line = async {
            match &mut lines {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };
        let answer = async {
            match replies.front_mut() {
                Some(rx) => rx.await.ok(),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            r = &mut running => return r,
            Some(u) = urls.recv() => {
                let shown = Instructions::new(&u, &view.env);
                for l in shown.lines(view.what, lines.is_some()) {
                    let l = if l == u && view.links { hyperlink(&u, &u) } else { l };
                    let _ = writeln!(err, "{l}");
                }
                if lines.is_some() {
                    let _ = writeln!(
                        err,
                        "{}",
                        if shown.no_browser.is_some() {
                            "Paste the address here and press Enter (or type c and Enter to copy the link):"
                        } else {
                            "Waiting for the browser… (if it runs on another machine, paste the address it ends up at here)"
                        }
                    );
                } else {
                    let _ = writeln!(err, "Waiting for the browser…");
                }
                url = Some(u);
            }
            Some(l) = line => {
                let l = zeroize::Zeroizing::new(l);
                match l.trim() {
                    "" => {}
                    "c" | "C" => {
                        if let Some(u) = &url {
                            let clip = harness_tui::clipboard::Clipboard::from_env(
                                harness_tui::clipboard::ClipboardMode::Osc52,
                                env,
                                &|_| false,
                            );
                            let _ = clip.copy(u, &mut |seq| err.write_all(seq.as_bytes()));
                            let _ = writeln!(err, "Copied the link (OSC 52). Paste the address here when you are done:");
                        }
                    }
                    text => {
                        let (p, rx) = Paste::new(text);
                        let _ = paste.send(p);
                        replies.push_back(rx);
                    }
                }
            }
            Some(r) = answer => {
                replies.pop_front();
                match r {
                    Ok(()) => { let _ = writeln!(err, "Got it; finishing…"); }
                    Err(e) => { let _ = writeln!(err, "{e}. Paste again:"); }
                }
            }
        }
    }
}

/// `harness login ...`. Returns the exit code.
pub async fn login_main(
    args: &[String],
    path: &Path,
    prompt: &mut dyn Prompt,
    verifier: &dyn Verifier,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    if args.first().is_some_and(|a| a == "mcp") {
        if args.iter().any(|a| a == "--help" || a == "-h") {
            let _ = writeln!(out, "{}", mcp::USAGE);
            return 0;
        }
        return mcp::login_main(&args[1..], path, prompt, verifier.browser(), env, out, err).await;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        let _ = writeln!(out, "{USAGE}");
        return 0;
    }
    let parsed = match parse_login(args) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(err, "harness login: {e}\n\n{USAGE}");
            return 2;
        }
    };
    let support = auth_support(parsed.provider).expect("provider was checked");
    let credential = if parsed.chatgpt {
        Credential::ChatGpt
    } else if let Some(cmd) = parsed.cmd {
        Credential::Command(cmd.trim().to_owned())
    } else if let Some(var) = parsed.env {
        Credential::Env(var.trim().to_owned())
    } else {
        let _ = writeln!(err, "{}: {}", support.label, support.note);
        let method = if prompt.interactive() {
            match choose_method(support.provider, prompt, err) {
                Ok(Some(m)) => m,
                Ok(None) => return cancelled(err),
                Err(e) => return io_error(err, &e),
            }
        } else {
            Method::ApiKey
        };
        let value = match method {
            Method::ChatGpt => Ok(Some(Secret::new("chatgpt".into()))),
            Method::ApiKey => {
                if prompt.interactive() {
                    let _ = writeln!(err, "Create a key at {}", support.key_url);
                }
                prompt.secret(&format!("{} API key (input is hidden): ", support.label))
            }
            Method::Command => prompt
                .line("Command that prints the key: ")
                .map(|l| l.map(Secret::new)),
            Method::Env => prompt
                .line(&format!("Environment variable [{}]: ", support.env_var))
                .map(|l| {
                    l.map(|v| {
                        Secret::new(if v.is_empty() {
                            support.env_var.to_owned()
                        } else {
                            v
                        })
                    })
                }),
        };
        match value {
            Ok(Some(v)) if !v.is_empty() => method.credential(v),
            Ok(_) => return cancelled(err),
            Err(e) => return io_error(err, &e),
        }
    };
    let request = LoginRequest {
        provider: support.provider,
        credential,
    };
    let result = if matches!(request.credential, Credential::ChatGpt) {
        let view = SignInView {
            what: "sign in with ChatGPT",
            env: Environment::from_env(env),
            links: prompt.hyperlinks(),
        };
        let (tx, urls) = tokio::sync::mpsc::unbounded_channel();
        let shown = TerminalBrowser {
            shown: tx,
            inner: verifier.browser(),
            open: view.env.opens_browser(),
        };
        let (paste, mut pasted) = paste_channel();
        let lines = prompt.paste_lines();
        drive_sign_in(
            apply_with(path, &request, verifier, env, &shown, &mut pasted),
            urls,
            lines,
            paste,
            &view,
            env,
            err,
        )
        .await
    } else {
        let _ = writeln!(err, "Checking the key with {}…", support.label);
        apply(path, &request, verifier, env).await
    };
    match result {
        Ok(summary) => {
            let _ = writeln!(out, "{summary}");
            0
        }
        Err(e) => {
            let _ = writeln!(err, "harness login: {e}");
            1
        }
    }
}

fn cancelled(err: &mut dyn Write) -> i32 {
    let _ = writeln!(err, "harness login: cancelled; nothing was saved");
    1
}

fn io_error(err: &mut dyn Write, e: &std::io::Error) -> i32 {
    let _ = writeln!(err, "harness login: cannot read input: {e}");
    1
}

/// `harness logout <provider> [--yes]`. Returns the exit code.
pub fn logout_main(
    args: &[String],
    path: &Path,
    prompt: &mut dyn Prompt,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    if args.first().is_some_and(|a| a == "mcp") {
        if args.iter().any(|a| a == "--help" || a == "-h") {
            let _ = writeln!(out, "{}", mcp::USAGE);
            return 0;
        }
        return mcp::logout_main(&args[1..], path, prompt, out, err);
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        let _ = writeln!(out, "{USAGE}");
        return 0;
    }
    let yes = args.iter().any(|a| a == "--yes" || a == "-y");
    let rest: Vec<&String> = args
        .iter()
        .filter(|a| *a != "--yes" && *a != "-y")
        .collect();
    let provider = match rest.as_slice() {
        [p] if !p.starts_with('-') => known_provider(Some(p)),
        [] => Err("a provider is required".into()),
        _ => Err(format!(
            "unexpected arguments: {}",
            rest.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        )),
    };
    let provider = match provider {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(err, "harness logout: {e}\n\n{USAGE}");
            return 2;
        }
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            let _ = writeln!(err, "harness logout: {}: {e}", path.display());
            return 1;
        }
    };
    let keys = edit::credential_keys(&text, provider);
    // OpenAI may also have ChatGPT tokens in the data dir's token store.
    let store = (provider == harness_providers::openai::PROVIDER_NAME)
        .then(|| {
            harness_config::ini::load_str(&text, Some(path))
                .map(|cfg| crate::wiring::chatgpt_store(&cfg))
                .map_err(|e| format!("{}:\n{e}", path.display()))
        })
        .transpose();
    let store = match store {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(err, "harness logout: {e}");
            return 1;
        }
    };
    let tokens = store
        .as_ref()
        .filter(|s| s.load().is_ok_and(|r| r.tokens.is_some()));
    if keys.is_empty() && tokens.is_none() {
        let _ = writeln!(
            out,
            "no stored credential for {provider} in {}",
            path.display()
        );
        return 0;
    }
    if !yes {
        if !prompt.interactive() {
            let _ = writeln!(
                err,
                "harness logout: stdin is not a terminal; pass --yes to confirm"
            );
            return 2;
        }
        let mut what = Vec::new();
        if !keys.is_empty() {
            what.push(format!(
                "{} from [provider.{provider}] in {}",
                keys.join(", "),
                path.display()
            ));
        }
        if let Some(s) = tokens {
            what.push(format!("the ChatGPT tokens in {}", s.dir().display()));
        }
        let q = format!("Remove {}? [y/N] ", what.join(" and "));
        match prompt.line(&q) {
            Ok(Some(a)) if matches!(a.to_ascii_lowercase().as_str(), "y" | "yes") => {}
            Ok(_) => {
                let _ = writeln!(err, "harness logout: cancelled; nothing was changed");
                return 1;
            }
            Err(e) => {
                let _ = writeln!(err, "harness logout: cannot read input: {e}");
                return 1;
            }
        }
    }
    if let Some(s) = tokens {
        match s.clear_tokens() {
            Ok(_) => {
                let _ = writeln!(out, "deleted the ChatGPT tokens in {}", s.dir().display());
            }
            Err(e) => {
                let _ = writeln!(err, "harness logout: {e}");
                return 1;
            }
        }
    }
    if keys.is_empty() {
        return 0;
    }
    match edit::remove_credential_file(path, provider) {
        Ok(removed) => {
            let _ = writeln!(
                out,
                "removed {} from [provider.{provider}] in {}",
                removed.join(", "),
                path.display()
            );
            0
        }
        Err(e) => {
            let _ = writeln!(err, "harness logout: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct Script {
        tty: bool,
        answers: VecDeque<&'static str>,
        prompts: Vec<String>,
    }

    impl Script {
        fn new(tty: bool, answers: &[&'static str]) -> Self {
            Self {
                tty,
                answers: answers.iter().copied().collect(),
                prompts: Vec::new(),
            }
        }
    }

    impl Prompt for Script {
        fn interactive(&self) -> bool {
            self.tty
        }
        fn secret(&mut self, p: &str) -> std::io::Result<Option<Secret>> {
            self.prompts.push(p.to_owned());
            Ok(self.answers.pop_front().map(|a| Secret::new(a.into())))
        }
        fn line(&mut self, p: &str) -> std::io::Result<Option<String>> {
            self.prompts.push(p.to_owned());
            Ok(self.answers.pop_front().map(str::to_owned))
        }
    }

    /// Accepts only `good`; records what it saw.
    struct Fake {
        good: &'static str,
        seen: Mutex<Vec<(String, String, Option<String>)>>,
    }

    impl Fake {
        fn new(good: &'static str) -> Self {
            Self {
                good,
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Verifier for Fake {
        async fn verify(
            &self,
            provider: &str,
            key: &Secret,
            base_url: Option<&str>,
        ) -> Result<KeyCheck, ProviderError> {
            self.seen.lock().unwrap().push((
                provider.into(),
                key.expose().into(),
                base_url.map(str::to_owned),
            ));
            if key.expose() == self.good {
                Ok(KeyCheck {
                    provider: provider.into(),
                    url: "http://x".into(),
                    detail: Some("3 models visible".into()),
                })
            } else {
                Err(ProviderError::Auth("invalid x-api-key".into()))
            }
        }
    }

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    const INI: &str = "; keep me\n[general]\ndefault_model = opus\n\n[provider.anthropic]\nbase_url = http://mock.local\n";

    async fn run(
        a: &str,
        path: &Path,
        p: &mut Script,
        v: &Fake,
        env: &(dyn Fn(&str) -> Option<String> + Sync),
    ) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let rc = login_main(&args(a), path, p, v, env, &mut out, &mut err).await;
        (
            rc,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[tokio::test]
    async fn login_verifies_then_saves_inline_key_with_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness.ini");
        std::fs::write(&path, INI).unwrap();
        let v = Fake::new("sk-ant-good");
        let mut p = Script::new(true, &["", "sk-ant-good"]);
        let (rc, out, err) = run("anthropic", &path, &mut p, &v, &no_env).await;
        assert_eq!(rc, 0, "{err}");
        assert!(
            out.contains("Anthropic accepted the key (3 models visible); saved api_key"),
            "{out}"
        );
        assert!(err.contains("subscription login"), "{err}");
        assert!(p.prompts[1].contains("input is hidden"));
        for text in [&out, &err] {
            assert!(!text.contains("sk-ant-good"), "key leaked: {text}");
        }
        assert_eq!(
            v.seen.lock().unwrap()[0],
            (
                "anthropic".into(),
                "sk-ant-good".into(),
                Some("http://mock.local".into())
            )
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, format!("{INI}api_key = sk-ant-good\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[tokio::test]
    async fn rejected_key_is_not_saved_and_explains_why() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness.ini");
        std::fs::write(&path, INI).unwrap();
        let v = Fake::new("sk-ant-good");
        // Not a terminal: the key is read from stdin, no method menu.
        let mut p = Script::new(false, &["sk-ant-wrong"]);
        let (rc, _, err) = run("anthropic", &path, &mut p, &v, &no_env).await;
        assert_eq!(rc, 1);
        assert!(
            err.contains("Anthropic rejected the key (invalid x-api-key)"),
            "{err}"
        );
        assert!(err.contains("console.anthropic.com"), "{err}");
        assert!(err.contains("Nothing was saved"), "{err}");
        assert!(!err.contains("sk-ant-wrong"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), INI);
    }

    #[tokio::test]
    async fn api_key_cmd_and_env_are_checked_then_stored_by_reference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("harness.ini");
        let v = Fake::new("sk-or-good");
        let mut p = Script::new(false, &[]);
        // The command is one argument, so build the args by hand.
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let rc = login_main(
            &[
                "openrouter".into(),
                "--api-key-cmd".into(),
                "echo sk-or-good".into(),
            ],
            &path,
            &mut p,
            &v,
            &no_env,
            &mut o,
            &mut e,
        )
        .await;
        assert_eq!(rc, 0, "{}", String::from_utf8_lossy(&e));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[provider.openrouter]\napi_key_cmd = echo sk-or-good\n"
        );

        let env = |k: &str| (k == "MY_OR_KEY").then(|| "sk-or-good".to_owned());
        let (rc, _, err) = run(
            "openrouter --api-key-env MY_OR_KEY",
            &path,
            &mut p,
            &v,
            &env,
        )
        .await;
        assert_eq!(rc, 0, "{err}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[provider.openrouter]\napi_key_env = MY_OR_KEY\n",
            "the env reference replaces the command"
        );

        let (rc, _, err) = run(
            "openrouter --api-key-env UNSET_VAR",
            &path,
            &mut p,
            &v,
            &env,
        )
        .await;
        assert_eq!(rc, 1);
        assert!(err.contains("UNSET_VAR is not set"), "{err}");
    }

    #[tokio::test]
    async fn interactive_menu_chooses_env_with_default_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness.ini");
        let v = Fake::new("sk-oa");
        let env = |k: &str| (k == "OPENAI_API_KEY").then(|| "sk-oa".to_owned());
        let mut p = Script::new(true, &["9", "3", ""]);
        let (rc, _, err) = run("openai", &path, &mut p, &v, &env).await;
        assert_eq!(rc, 0, "{err}");
        assert!(err.contains("enter 1, 2 or 3"));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("api_key_env = OPENAI_API_KEY")
        );
    }

    #[tokio::test]
    async fn usage_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness.ini");
        let v = Fake::new("x");
        let mut p = Script::new(false, &[]);
        for a in [
            "",
            "gemini",
            "openai --bogus",
            "openai --api-key-cmd a --api-key-env B",
            "openai extra",
        ] {
            let (rc, _, err) = run(a, &path, &mut p, &v, &no_env).await;
            assert_eq!(rc, 2, "{a}: {err}");
            assert!(err.contains("usage: harness login"), "{err}");
        }
        let (rc, _, err) = run("openai", &path, &mut p, &v, &no_env).await;
        assert_eq!(rc, 1, "EOF on stdin cancels");
        assert!(err.contains("cancelled"), "{err}");
        assert!(!path.exists());
    }

    #[test]
    fn logout_asks_and_removes_all_credential_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness.ini");
        let ini = "[provider.openai]\napi_key = sk-1\napi_key_env = X\nbase_url = u\n[provider.anthropic]\napi_key = sk-2\n";
        std::fs::write(&path, ini).unwrap();
        let logout = |a: &str, p: &mut Script| {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let rc = logout_main(&args(a), &path, p, &mut out, &mut err);
            (
                rc,
                String::from_utf8(out).unwrap() + &String::from_utf8(err).unwrap(),
            )
        };
        let mut p = Script::new(true, &["n"]);
        let (rc, text) = logout("openai", &mut p);
        assert_eq!(rc, 1, "{text}");
        assert!(p.prompts[0].contains("Remove api_key, api_key_env from [provider.openai]"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), ini);

        let mut p = Script::new(false, &[]);
        let (rc, text) = logout("openai", &mut p);
        assert_eq!(rc, 2);
        assert!(text.contains("--yes"));

        let mut p = Script::new(true, &["y"]);
        let (rc, text) = logout("openai", &mut p);
        assert_eq!(rc, 0, "{text}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[provider.openai]\nbase_url = u\n[provider.anthropic]\napi_key = sk-2\n"
        );
        let (rc, text) = logout("openai", &mut Script::new(false, &[]));
        assert_eq!(rc, 0);
        assert!(text.contains("no stored credential"));
        let (rc, _) = logout("anthropic --yes", &mut Script::new(false, &[]));
        assert_eq!(rc, 0);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("sk-2"));
        assert_eq!(logout("", &mut Script::new(false, &[])).0, 2);
        assert_eq!(logout("nope", &mut Script::new(false, &[])).0, 2);
    }

    #[tokio::test]
    async fn apply_is_what_the_tui_flow_calls() {
        use harness_config::login::{LoginEvent, LoginFlow, LoginKey};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness.ini");
        let mut flow = LoginFlow::for_provider("openrouter").unwrap();
        flow.handle(LoginKey::Enter);
        flow.handle(LoginKey::Paste("sk-or-flow".into()));
        let LoginEvent::Submit(req) = flow.handle(LoginKey::Enter) else {
            panic!()
        };
        let v = Fake::new("sk-or-flow");
        let r = apply(&path, &req, &v, &no_env).await;
        assert!(
            r.as_ref().unwrap().contains("OpenRouter accepted the key"),
            "{r:?}"
        );
        flow.finish(r);
        assert_eq!(flow.step(), harness_config::login::LoginStep::Done);
        assert!(!flow.message().unwrap().contains("sk-or-flow"));
    }
}

#[cfg(test)]
pub(crate) mod chatgpt_tests {
    use super::*;
    use harness_providers::chatgpt::mock::{MockAuthServer, MockBrowser, MockConfig};
    use std::time::Duration;

    /// Signs in against the mock server, following the consent page with
    /// a [`MockBrowser`]. Never checks API keys.
    pub(crate) struct MockSignIn {
        pub(crate) server: MockAuthServer,
        pub(crate) browser: MockBrowser,
    }

    impl MockSignIn {
        pub(crate) async fn start(config: MockConfig) -> Self {
            Self {
                server: MockAuthServer::start(config).await,
                browser: MockBrowser::new(),
            }
        }
    }

    #[async_trait]
    impl Verifier for MockSignIn {
        async fn verify(
            &self,
            _: &str,
            _: &Secret,
            _: Option<&str>,
        ) -> Result<KeyCheck, ProviderError> {
            Err(ProviderError::Auth("no API keys in this test".into()))
        }

        fn browser(&self) -> &dyn Browser {
            &self.browser
        }

        async fn sign_in_chatgpt(
            &self,
            store: &TokenStore,
            _: &Endpoints,
            browser: &dyn Browser,
            paste: &mut PasteReceiver,
        ) -> Result<Account, ChatGptError> {
            SignIn::new(store.clone())
                .with_endpoints(self.server.endpoints())
                .with_port(0)
                .with_timeout(Duration::from_secs(10))
                .run_with(browser, paste)
                .await
        }
    }

    struct Script(bool, Vec<&'static str>);

    impl Prompt for Script {
        fn interactive(&self) -> bool {
            self.0
        }
        fn secret(&mut self, _: &str) -> std::io::Result<Option<Secret>> {
            Ok(None)
        }
        fn line(&mut self, _: &str) -> std::io::Result<Option<String>> {
            Ok((!self.1.is_empty()).then(|| self.1.remove(0).to_owned()))
        }
    }

    /// A desktop session: the browser opens here.
    fn no_env(k: &str) -> Option<String> {
        (k == "DISPLAY").then(|| ":0".to_owned())
    }

    fn setup() -> (tempfile::TempDir, PathBuf, TokenStore) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("harness.ini");
        let data = tmp.path().join("data");
        std::fs::write(
            &path,
            format!(
                "[general]\ndata_dir = {}\n\n[provider.openai]\napi_key_env = OPENAI_API_KEY\n",
                data.display()
            ),
        )
        .unwrap();
        (tmp, path, TokenStore::new(data.join("auth")))
    }

    async fn login(
        args: &[&str],
        path: &Path,
        prompt: &mut Script,
        v: &MockSignIn,
    ) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let rc = login_main(&args, path, prompt, v, &no_env, &mut out, &mut err).await;
        (
            rc,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[tokio::test]
    async fn login_openai_chatgpt_signs_in_and_sets_auth() {
        let (_tmp, path, store) = setup();
        let v = MockSignIn::start(MockConfig::default()).await;
        let (rc, out, err) = login(
            &["openai", "--chatgpt"],
            &path,
            &mut Script(false, vec![]),
            &v,
        )
        .await;
        assert_eq!(rc, 0, "{err}");
        assert!(
            out.contains("signed in with ChatGPT as user@example.com"),
            "{out}"
        );
        assert!(err.contains("Opening your browser"), "{err}");
        assert!(
            err.contains("/api/accounts/authorize?"),
            "the URL is shown: {err}"
        );
        let ini = std::fs::read_to_string(&path).unwrap();
        assert!(ini.contains("auth = chatgpt"), "{ini}");
        assert!(
            ini.contains("api_key_env = OPENAI_API_KEY"),
            "API key kept: {ini}"
        );
        let cfg = harness_config::ini::load_str(&ini, None).unwrap();
        assert!(crate::wiring::openai_uses_chatgpt(&cfg));

        let tokens = store.load().unwrap().tokens.unwrap();
        for text in [&out, &err, &ini] {
            assert!(
                !text.contains(tokens.access_token.expose()),
                "token leaked: {text}"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.auth_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Logout deletes the tokens and the auth line, keeping the
        // registration.
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let rc = logout_main(
            &["openai".into(), "--yes".into()],
            &path,
            &mut Script(false, vec![]),
            &mut o,
            &mut e,
        );
        let o = String::from_utf8(o).unwrap();
        assert_eq!(rc, 0, "{}", String::from_utf8_lossy(&e));
        assert!(o.contains("deleted the ChatGPT tokens"), "{o}");
        assert!(o.contains("api_key_env, auth"), "{o}");
        let r = store.load().unwrap();
        assert!(r.tokens.is_none());
        assert_eq!(r.client_id.as_deref(), Some("oaiapp_mock123"));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("auth ="));
    }

    #[tokio::test]
    async fn interactive_menu_offers_both_methods_for_openai() {
        let (_tmp, path, store) = setup();
        let v = MockSignIn::start(MockConfig::default()).await;
        let (rc, _, err) = login(&["openai"], &path, &mut Script(true, vec!["7", "4"]), &v).await;
        assert_eq!(rc, 0, "{err}");
        assert!(err.contains("1) Paste an API key"), "{err}");
        assert!(err.contains("4) Sign in with ChatGPT"), "{err}");
        assert!(err.contains("or 4 to sign in with ChatGPT"), "{err}");
        assert!(store.load().unwrap().tokens.is_some());
    }

    #[tokio::test]
    async fn refused_sign_ins_change_nothing() {
        for (config, expect) in [
            (
                MockConfig {
                    deny: true,
                    ..MockConfig::default()
                },
                "declined",
            ),
            (
                MockConfig {
                    grant_plan: false,
                    ..MockConfig::default()
                },
                "plan usage was not granted",
            ),
        ] {
            let (_tmp, path, store) = setup();
            let before = std::fs::read_to_string(&path).unwrap();
            let v = MockSignIn::start(config).await;
            let (rc, _, err) = login(
                &["openai", "--chatgpt"],
                &path,
                &mut Script(false, vec![]),
                &v,
            )
            .await;
            assert_eq!(rc, 1);
            assert!(err.contains(expect), "{err}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
            assert!(store.load().unwrap().tokens.is_none());
        }
    }

    #[tokio::test]
    async fn chatgpt_usage_errors() {
        let (_tmp, path, _) = setup();
        let v = MockSignIn::start(MockConfig::default()).await;
        for args in [
            &["anthropic", "--chatgpt"][..],
            &["openai", "--chatgpt", "--api-key-env", "X"][..],
        ] {
            let (rc, _, err) = login(args, &path, &mut Script(false, vec![]), &v).await;
            assert_eq!(rc, 2, "{args:?}: {err}");
        }
    }

    #[tokio::test]
    async fn tui_flow_submits_chatgpt_through_apply() {
        use harness_config::login::{LoginEvent, LoginFlow, LoginStep};
        let (_tmp, path, _) = setup();
        let v = MockSignIn::start(MockConfig::default()).await;
        let mut flow = LoginFlow::for_provider("openai").unwrap();
        let LoginEvent::Submit(req) = flow.click(3) else {
            panic!("expected submit")
        };
        let r = apply(&path, &req, &v, &no_env).await;
        assert!(
            r.as_ref().unwrap().contains("signed in with ChatGPT"),
            "{r:?}"
        );
        assert_eq!(v.browser.opened().len(), 1);
        flow.finish(r);
        assert_eq!(flow.step(), LoginStep::Done);
    }

    /// A writer the test can read while the login runs.
    #[derive(Clone, Default)]
    pub(super) struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Shared {
        pub(super) fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }

        /// Waits for the first printed authorization URL.
        pub(super) async fn url(&self, path: &str) -> String {
            for _ in 0..1000 {
                if let Some(l) = self.text().lines().find(|l| l.contains(path)) {
                    // The plain URL sits inside the OSC 8 hyperlink.
                    let start = l.find("http").unwrap();
                    let end = l[start..].find('\x1b').map_or(l.len(), |e| start + e);
                    return l[start..end].to_owned();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("no URL printed: {}", self.text());
        }
    }

    /// A terminal where the user types `lines` while the sign-in waits.
    pub(super) struct Typing(pub(super) Option<UnboundedReceiver<String>>);

    impl Prompt for Typing {
        fn interactive(&self) -> bool {
            true
        }
        fn secret(&mut self, _: &str) -> std::io::Result<Option<Secret>> {
            Ok(None)
        }
        fn line(&mut self, _: &str) -> std::io::Result<Option<String>> {
            Ok(None)
        }
        fn hyperlinks(&self) -> bool {
            true
        }
        fn paste_lines(&mut self) -> Option<UnboundedReceiver<String>> {
            self.0.take()
        }
    }

    pub(super) fn over_ssh(k: &str) -> Option<String> {
        match k {
            "SSH_CONNECTION" => Some("10.0.0.7 50122 10.0.0.2 22".into()),
            "USER" => Some("dev".into()),
            _ => None,
        }
    }

    #[tokio::test]
    async fn over_ssh_the_link_is_shown_and_a_pasted_address_signs_in() {
        let (_tmp, path, store) = setup();
        let v = MockSignIn::start(MockConfig::default()).await;
        let (typed, lines) = tokio::sync::mpsc::unbounded_channel();
        let mut prompt = Typing(Some(lines));
        let err = Shared::default();
        let mut err_w = err.clone();
        let mut out = Vec::new();
        let args = vec!["openai".to_owned(), "--chatgpt".to_owned()];
        let login = login_main(
            &args,
            &path,
            &mut prompt,
            &v,
            &over_ssh,
            &mut out,
            &mut err_w,
        );
        let user = async {
            let url = err.url("/api/accounts/authorize?").await;
            // The browser on the user's own machine.
            let elsewhere = MockBrowser::remote();
            elsewhere.open(&url).unwrap();
            let address = elsewhere.next_redirect().await;
            typed.send("c".into()).unwrap();
            typed.send("not an address".into()).unwrap();
            typed.send(address).unwrap();
        };
        let (rc, ()) = tokio::join!(login, user);
        let err = err.text();
        assert_eq!(rc, 0, "{err}");
        assert!(v.browser.opened().is_empty(), "no local browser over SSH");
        assert!(
            err.contains("Not opening a browser here (this is an SSH session)"),
            "{err}"
        );
        let url_line = err
            .lines()
            .find(|l| l.contains("/api/accounts/authorize?"))
            .unwrap();
        assert!(
            url_line.starts_with("\x1b]8;;http"),
            "OSC 8 link: {url_line:?}"
        );
        let redirect = v.server.requests_to("/api/accounts/authorize")[0]
            .param("redirect_uri")
            .unwrap()
            .to_owned();
        let port: u16 = redirect
            .rsplit(':')
            .next()
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            err.contains(&format!("ssh -N -L {port}:127.0.0.1:{port} dev@10.0.0.2")),
            "{err}"
        );
        assert!(err.contains("\x1b]52;c;"), "c copies the link: {err:?}");
        assert!(err.contains("Paste again"), "{err}");
        assert!(err.contains("Got it"), "{err}");
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("signed in with ChatGPT"), "{out}");
        assert!(store.load().unwrap().tokens.is_some());
    }

    #[tokio::test]
    async fn without_a_terminal_over_ssh_the_link_is_still_printed_plainly() {
        let (_tmp, path, _) = setup();
        let v = MockSignIn::start(MockConfig::default()).await;
        let err = Shared::default();
        let mut err_w = err.clone();
        let mut out = Vec::new();
        let args = vec!["openai".to_owned(), "--chatgpt".to_owned()];
        // Nothing comes back: the flow times out (10s in this mock); stop
        // early once the link is out.
        let mut prompt = Script(false, vec![]);
        let login = login_main(
            &args,
            &path,
            &mut prompt,
            &v,
            &over_ssh,
            &mut out,
            &mut err_w,
        );
        tokio::select! {
            _ = login => panic!("nothing could finish this sign-in"),
            url = err.url("/api/accounts/authorize?") => {
                assert!(url.contains("state="), "{url}");
            }
        }
        let text = err.text();
        assert!(
            !text.contains('\x1b'),
            "no escapes without a terminal: {text:?}"
        );
        assert!(
            text.lines()
                .any(|l| l.starts_with("http") && l.contains("code_challenge="))
        );
        assert!(v.browser.opened().is_empty());
    }
}
