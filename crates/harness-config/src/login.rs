//! The provider login flow shared by `harness login` and the TUI `/login`
//! action: which auth each provider supports, masked key entry, and a small
//! keyboard- and mouse-driven state machine.
//!
//! The flow never touches the network or the file system. When the user
//! submits, [`LoginFlow::handle`] returns a [`LoginRequest`]; the caller
//! verifies the key with a cheap authenticated request, saves it with
//! [`crate::ini::edit::save_credential`] and reports the result back with
//! [`LoginFlow::finish`].
//!
//! Typed keys live in a [`MaskedInput`]: zeroized on drop, displayed only as
//! `•` characters, and never printed by `Debug`.
//!
//! ```
//! use harness_config::login::{LoginEvent, LoginFlow, LoginKey, LoginStep};
//!
//! let mut flow = LoginFlow::for_provider("openrouter").unwrap();
//! assert_eq!(flow.step(), LoginStep::Method);
//! flow.handle(LoginKey::Enter); // "API key" is the first method
//! for c in "sk-or-v1-abc".chars() {
//!     flow.handle(LoginKey::Char(c));
//! }
//! assert_eq!(flow.field().unwrap(), "••••••••••••");
//! let LoginEvent::Submit(req) = flow.handle(LoginKey::Enter) else { panic!() };
//! assert_eq!(req.provider, "openrouter");
//! flow.finish(Ok("saved".into()));
//! assert_eq!(flow.step(), LoginStep::Done);
//! ```

use zeroize::Zeroizing;

use crate::ini::edit::Credential;
use crate::secrets::Secret;

/// The character shown for each typed key character.
pub const MASK_CHAR: char = '•';

/// What one provider supports for authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthSupport {
    /// Provider id (`anthropic`, `openai`, `openrouter`).
    pub provider: &'static str,
    /// Display name.
    pub label: &'static str,
    /// Supported auth methods, for display.
    pub methods: &'static [&'static str],
    /// The environment variable conventionally holding the key.
    pub env_var: &'static str,
    /// Where to create a key.
    pub key_url: &'static str,
    /// Extra information shown with the methods.
    pub note: &'static str,
}

/// Supported auth per provider, in display order.
pub const AUTH_SUPPORT: [AuthSupport; 3] = [
    AuthSupport {
        provider: "anthropic",
        label: "Anthropic",
        methods: &["API key"],
        env_var: "ANTHROPIC_API_KEY",
        key_url: "https://console.anthropic.com/settings/keys",
        note: "API key only. Claude subscription login (Free, Pro, Max) is not supported: \
               Anthropic's terms do not allow third-party apps to offer Claude.ai login \
               or to route requests through subscription credentials.",
    },
    AuthSupport {
        provider: "openai",
        label: "OpenAI",
        methods: &["API key", "Sign in with ChatGPT"],
        env_var: "OPENAI_API_KEY",
        key_url: "https://platform.openai.com/api-keys",
        note: "API key, or Sign in with ChatGPT to use your ChatGPT Plus or Pro plan \
               (opens the browser; sets auth = chatgpt).",
    },
    AuthSupport {
        provider: "openrouter",
        label: "OpenRouter",
        methods: &["API key"],
        env_var: "OPENROUTER_API_KEY",
        key_url: "https://openrouter.ai/settings/keys",
        note: "API key only.",
    },
];

/// Auth support for `provider`, if harness knows it.
pub fn auth_support(provider: &str) -> Option<&'static AuthSupport> {
    AUTH_SUPPORT.iter().find(|a| a.provider == provider)
}

/// How the key is stored in `harness.ini`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Inline `api_key`; the file is made `0600`.
    ApiKey,
    /// `api_key_cmd`: a command printing the key (a password manager).
    Command,
    /// `api_key_env`: the name of an environment variable.
    Env,
    /// Sign in with ChatGPT in the browser (OpenAI only): `auth = chatgpt`
    /// plus tokens in the protected token store.
    ChatGpt,
}

impl Method {
    /// The API key methods every provider offers, in menu order.
    pub const ALL: [Method; 3] = [Method::ApiKey, Method::Command, Method::Env];

    /// OpenAI's methods: the API key methods, then Sign in with ChatGPT.
    pub const OPENAI: [Method; 4] = [
        Method::ApiKey,
        Method::Command,
        Method::Env,
        Method::ChatGpt,
    ];

    /// The methods `provider` offers, in menu order.
    pub fn for_provider(provider: &str) -> &'static [Method] {
        if provider == "openai" {
            &Self::OPENAI
        } else {
            &Self::ALL
        }
    }

    /// The `harness.ini` key.
    pub fn ini_key(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::Command => "api_key_cmd",
            Self::Env => "api_key_env",
            Self::ChatGpt => "auth",
        }
    }

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            Self::ApiKey => "Paste an API key",
            Self::Command => "Use a command that prints the key",
            Self::Env => "Read the key from an environment variable",
            Self::ChatGpt => "Sign in with ChatGPT",
        }
    }

    /// Menu detail.
    pub fn detail(self) -> &'static str {
        match self {
            Self::ApiKey => "stored as api_key in harness.ini (mode 0600)",
            Self::Command => "stored as api_key_cmd, e.g. `pass show anthropic`",
            Self::Env => "stored as api_key_env, e.g. ANTHROPIC_API_KEY",
            Self::ChatGpt => "use your ChatGPT plan; opens the browser, sets auth = chatgpt",
        }
    }

    /// True when typed input must be masked.
    pub fn masked(self) -> bool {
        self == Self::ApiKey
    }

    /// Prompt for the value.
    pub fn prompt(self) -> &'static str {
        match self {
            Self::ApiKey => "API key",
            Self::Command => "Command",
            Self::Env => "Variable name",
            Self::ChatGpt => "Sign in",
        }
    }

    /// Wraps `value` as the credential this method stores.
    pub fn credential(self, value: Secret) -> Credential {
        match self {
            Self::ApiKey => Credential::ApiKey(value),
            Self::Command => Credential::Command(value.expose().trim().to_owned()),
            Self::Env => Credential::Env(value.expose().trim().to_owned()),
            Self::ChatGpt => Credential::ChatGpt,
        }
    }
}

/// A text buffer for secrets: zeroized on drop, shown only as a mask.
#[derive(Default)]
pub struct MaskedInput(Zeroizing<String>);

impl std::fmt::Debug for MaskedInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MaskedInput({} chars)", self.len())
    }
}

impl MaskedInput {
    /// An empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a character. Control characters are ignored.
    pub fn push(&mut self, c: char) {
        if !c.is_control() {
            self.0.push(c);
        }
    }

    /// Appends pasted text, dropping line breaks and other control
    /// characters (a pasted key often ends with a newline).
    pub fn push_str(&mut self, s: &str) {
        for c in s.chars() {
            self.push(c);
        }
    }

    /// Removes the last character.
    pub fn backspace(&mut self) {
        self.0.pop();
    }

    /// Empties the buffer (the old contents are zeroized).
    pub fn clear(&mut self) {
        self.0 = Zeroizing::new(String::new());
    }

    /// Number of characters.
    pub fn len(&self) -> usize {
        self.0.chars().count()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// One [`MASK_CHAR`] per character.
    pub fn mask(&self) -> String {
        std::iter::repeat_n(MASK_CHAR, self.len()).collect()
    }

    /// The plain text, for non-secret inputs (commands, variable names).
    pub fn plain(&self) -> &str {
        &self.0
    }

    /// Takes the trimmed contents as a [`Secret`], leaving the buffer empty.
    pub fn take(&mut self) -> Secret {
        let s = Secret::new(self.0.trim().to_owned());
        self.clear();
        s
    }
}

/// Where the flow is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStep {
    /// Choosing a provider.
    Provider,
    /// Choosing how to store the key.
    Method,
    /// Typing the key, command or variable name.
    Value,
    /// Waiting for the caller to verify and save.
    Verifying,
    /// Saved.
    Done,
    /// Verification or saving failed; Enter retries.
    Failed,
}

/// Input to [`LoginFlow::handle`], mapped from the UI's keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginKey {
    /// Previous option.
    Up,
    /// Next option.
    Down,
    /// Confirm / activate.
    Enter,
    /// Back out one step.
    Esc,
    /// A typed character.
    Char(char),
    /// Delete the last character.
    Backspace,
    /// Pasted text.
    Paste(String),
}

/// What the caller should do after an input.
#[derive(Debug)]
pub enum LoginEvent {
    /// Nothing; re-render.
    None,
    /// Verify and save this request, then call [`LoginFlow::finish`].
    Submit(LoginRequest),
    /// The user backed out; close the flow.
    Cancelled,
    /// The flow finished; close it.
    Closed,
}

/// A credential to verify and save.
#[derive(Debug)]
pub struct LoginRequest {
    /// Provider id.
    pub provider: &'static str,
    /// What to store.
    pub credential: Credential,
}

/// One choice on a menu step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginOption {
    /// Main text.
    pub label: String,
    /// Secondary text.
    pub detail: String,
}

/// The login state machine. Render it with [`title`](Self::title),
/// [`options`](Self::options) / [`selected`](Self::selected),
/// [`field`](Self::field), [`message`](Self::message) and
/// [`hint`](Self::hint); drive it with [`handle`](Self::handle) for keys
/// and [`click`](Self::click) for mouse clicks on options.
#[derive(Debug)]
pub struct LoginFlow {
    step: LoginStep,
    preset: bool,
    provider: Option<&'static AuthSupport>,
    method: Option<Method>,
    cursor: usize,
    input: MaskedInput,
    message: Option<String>,
}

impl Default for LoginFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginFlow {
    /// A flow that starts by choosing the provider.
    pub fn new() -> Self {
        Self {
            step: LoginStep::Provider,
            preset: false,
            provider: None,
            method: None,
            cursor: 0,
            input: MaskedInput::new(),
            message: None,
        }
    }

    /// A flow for one provider, starting at the method choice. `None` for
    /// an unknown provider.
    pub fn for_provider(provider: &str) -> Option<Self> {
        let support = auth_support(provider)?;
        Some(Self {
            step: LoginStep::Method,
            preset: true,
            provider: Some(support),
            ..Self::new()
        })
    }

    /// Current step.
    pub fn step(&self) -> LoginStep {
        self.step
    }

    /// The chosen provider.
    pub fn provider(&self) -> Option<&'static AuthSupport> {
        self.provider
    }

    /// The chosen method.
    pub fn method(&self) -> Option<Method> {
        self.method
    }

    /// Heading for the current step.
    pub fn title(&self) -> String {
        let p = self.provider.map_or("", |p| p.label);
        match self.step {
            LoginStep::Provider => "Log in to a provider".into(),
            LoginStep::Method => format!("{p}: how should harness get the key?"),
            LoginStep::Value => format!("{p}: {}", self.method.map_or("", Method::prompt)),
            LoginStep::Verifying if self.method == Some(Method::ChatGpt) => {
                format!("{p}: finish signing in with ChatGPT in your browser…")
            }
            LoginStep::Verifying => format!("{p}: checking the key…"),
            LoginStep::Done => format!("{p}: logged in"),
            LoginStep::Failed => format!("{p}: login failed"),
        }
    }

    /// Key hint line for the current step.
    pub fn hint(&self) -> &'static str {
        match self.step {
            LoginStep::Provider | LoginStep::Method => "↑/↓ choose · Enter select · Esc back",
            LoginStep::Value => "Enter submit · Esc back",
            LoginStep::Verifying => "Esc cancel",
            LoginStep::Done => "Enter close",
            LoginStep::Failed => "Enter try again · Esc close",
        }
    }

    /// The options of a menu step (empty on other steps).
    pub fn options(&self) -> Vec<LoginOption> {
        match self.step {
            LoginStep::Provider => AUTH_SUPPORT
                .iter()
                .map(|a| LoginOption {
                    label: a.label.into(),
                    detail: a.methods.join(", "),
                })
                .collect(),
            LoginStep::Method => self
                .methods()
                .iter()
                .map(|m| LoginOption {
                    label: m.label().into(),
                    detail: m.detail().into(),
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Index of the highlighted option.
    pub fn selected(&self) -> usize {
        self.cursor
    }

    /// The text field as it should be displayed: masked for API keys,
    /// plain for commands and variable names. `None` off the value step.
    pub fn field(&self) -> Option<String> {
        if self.step != LoginStep::Value {
            return None;
        }
        Some(match self.method {
            Some(m) if !m.masked() => self.input.plain().to_owned(),
            _ => self.input.mask(),
        })
    }

    /// A status or error line (never contains the key).
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Provider note shown on the method step (for example why Anthropic is
    /// API key only).
    pub fn note(&self) -> Option<&'static str> {
        matches!(self.step, LoginStep::Method | LoginStep::Value)
            .then(|| self.provider.map(|p| p.note))
            .flatten()
    }

    fn option_count(&self) -> usize {
        match self.step {
            LoginStep::Provider => AUTH_SUPPORT.len(),
            LoginStep::Method => self.methods().len(),
            _ => 0,
        }
    }

    /// The methods of the chosen provider.
    fn methods(&self) -> &'static [Method] {
        Method::for_provider(self.provider.map_or("", |p| p.provider))
    }

    /// Starts Sign in with ChatGPT: there is nothing to type, the caller
    /// runs the browser sign-in.
    fn submit_chatgpt(&mut self) -> LoginEvent {
        let Some(p) = self.provider else {
            return LoginEvent::None;
        };
        self.method = Some(Method::ChatGpt);
        self.step = LoginStep::Verifying;
        self.message = None;
        LoginEvent::Submit(LoginRequest {
            provider: p.provider,
            credential: Credential::ChatGpt,
        })
    }

    /// Activates option `index` of a menu step, as a mouse click does.
    pub fn click(&mut self, index: usize) -> LoginEvent {
        if index >= self.option_count() {
            return LoginEvent::None;
        }
        self.cursor = index;
        self.handle(LoginKey::Enter)
    }

    fn enter_value(&mut self, method: Method) {
        self.method = Some(method);
        self.step = LoginStep::Value;
        self.input.clear();
        self.message = None;
        if method == Method::Env
            && let Some(p) = self.provider
        {
            self.input.push_str(p.env_var);
        }
    }

    /// Handles one input.
    pub fn handle(&mut self, key: LoginKey) -> LoginEvent {
        match (self.step, key) {
            (LoginStep::Provider | LoginStep::Method, LoginKey::Up) => {
                let n = self.option_count();
                self.cursor = (self.cursor + n - 1) % n;
            }
            (LoginStep::Provider | LoginStep::Method, LoginKey::Down) => {
                self.cursor = (self.cursor + 1) % self.option_count();
            }
            (LoginStep::Provider, LoginKey::Enter) => {
                self.provider = AUTH_SUPPORT.get(self.cursor);
                self.step = LoginStep::Method;
                self.cursor = 0;
            }
            (LoginStep::Provider, LoginKey::Esc) => return LoginEvent::Cancelled,
            (LoginStep::Method, LoginKey::Enter) => match self.methods()[self.cursor] {
                Method::ChatGpt => return self.submit_chatgpt(),
                m => self.enter_value(m),
            },
            (LoginStep::Method, LoginKey::Esc) => {
                if self.preset {
                    return LoginEvent::Cancelled;
                }
                self.step = LoginStep::Provider;
                self.cursor = AUTH_SUPPORT
                    .iter()
                    .position(|a| Some(a) == self.provider)
                    .unwrap_or(0);
            }
            (LoginStep::Value, LoginKey::Char(c)) => self.input.push(c),
            (LoginStep::Value, LoginKey::Paste(s)) => self.input.push_str(&s),
            (LoginStep::Value, LoginKey::Backspace) => self.input.backspace(),
            (LoginStep::Value, LoginKey::Esc) => {
                self.input.clear();
                self.step = LoginStep::Method;
                self.cursor = self
                    .method
                    .and_then(|m| self.methods().iter().position(|x| *x == m))
                    .unwrap_or(0);
                self.message = None;
            }
            (LoginStep::Value, LoginKey::Enter) => {
                let (Some(p), Some(m)) = (self.provider, self.method) else {
                    return LoginEvent::None;
                };
                if self.input.plain().trim().is_empty() {
                    self.message = Some(format!("enter the {}", m.prompt().to_lowercase()));
                    return LoginEvent::None;
                }
                let credential = m.credential(self.input.take());
                if let Err(e) = credential.validate() {
                    self.message = Some(e.to_string());
                    return LoginEvent::None;
                }
                self.step = LoginStep::Verifying;
                self.message = None;
                return LoginEvent::Submit(LoginRequest {
                    provider: p.provider,
                    credential,
                });
            }
            (LoginStep::Verifying, LoginKey::Esc) => return LoginEvent::Cancelled,
            (LoginStep::Done, LoginKey::Enter | LoginKey::Esc) => return LoginEvent::Closed,
            (LoginStep::Failed, LoginKey::Enter) if self.method == Some(Method::ChatGpt) => {
                return self.submit_chatgpt();
            }
            (LoginStep::Failed, LoginKey::Enter) => {
                if let Some(m) = self.method {
                    let msg = self.message.take();
                    self.enter_value(m);
                    self.message = msg;
                }
            }
            (LoginStep::Failed, LoginKey::Esc) => return LoginEvent::Cancelled,
            _ => {}
        }
        LoginEvent::None
    }

    /// Reports the outcome of a submitted request: `Ok(summary)` or
    /// `Err(message)`. Messages must not contain the key.
    pub fn finish(&mut self, result: Result<String, String>) {
        if self.step != LoginStep::Verifying {
            return;
        }
        match result {
            Ok(m) => {
                self.step = LoginStep::Done;
                self.message = Some(m);
            }
            Err(e) => {
                self.step = LoginStep::Failed;
                self.message = Some(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(flow: &mut LoginFlow, s: &str) {
        for c in s.chars() {
            flow.handle(LoginKey::Char(c));
        }
    }

    #[test]
    fn keyboard_path_masks_and_submits() {
        let mut f = LoginFlow::new();
        assert_eq!(f.step(), LoginStep::Provider);
        assert_eq!(f.options().len(), 3);
        f.handle(LoginKey::Down);
        f.handle(LoginKey::Down);
        f.handle(LoginKey::Down); // wraps to anthropic
        f.handle(LoginKey::Up); // openrouter
        assert_eq!(f.selected(), 2);
        f.handle(LoginKey::Enter);
        assert_eq!(f.provider().unwrap().provider, "openrouter");
        assert_eq!(f.step(), LoginStep::Method);
        f.handle(LoginKey::Enter);
        assert_eq!(f.method(), Some(Method::ApiKey));
        typed(&mut f, "sk-or-secret");
        f.handle(LoginKey::Backspace);
        f.handle(LoginKey::Paste("X\n".into()));
        assert_eq!(f.field().unwrap(), "•".repeat(12));
        let dbg = format!("{f:?}");
        assert!(!dbg.contains("secret"), "{dbg}");
        let LoginEvent::Submit(req) = f.handle(LoginKey::Enter) else {
            panic!("expected submit")
        };
        assert_eq!(req.provider, "openrouter");
        let Credential::ApiKey(k) = &req.credential else {
            panic!()
        };
        assert_eq!(k.expose(), "sk-or-secreX");
        assert!(!format!("{req:?}").contains("secre"));
        assert_eq!(f.step(), LoginStep::Verifying);
        f.finish(Err("the key was rejected".into()));
        assert_eq!(f.step(), LoginStep::Failed);
        f.handle(LoginKey::Enter);
        assert_eq!(f.step(), LoginStep::Value);
        assert_eq!(f.field().unwrap(), "", "retry starts empty");
        assert_eq!(f.message(), Some("the key was rejected"));
    }

    #[test]
    fn mouse_path_selects_options() {
        let mut f = LoginFlow::new();
        assert!(matches!(f.click(9), LoginEvent::None));
        f.click(1);
        assert_eq!(f.provider().unwrap().provider, "openai");
        f.click(2);
        assert_eq!(f.method(), Some(Method::Env));
        assert_eq!(
            f.field().unwrap(),
            "OPENAI_API_KEY",
            "prefilled, not masked"
        );
        let LoginEvent::Submit(req) = f.handle(LoginKey::Enter) else {
            panic!()
        };
        assert!(matches!(req.credential, Credential::Env(ref v) if v == "OPENAI_API_KEY"));
        f.finish(Ok("saved".into()));
        assert_eq!(f.step(), LoginStep::Done);
        assert!(matches!(f.handle(LoginKey::Enter), LoginEvent::Closed));
    }

    #[test]
    fn esc_backs_out_step_by_step() {
        let mut f = LoginFlow::new();
        f.click(0);
        f.click(1);
        assert_eq!(f.step(), LoginStep::Value);
        assert!(!f.method().unwrap().masked());
        f.handle(LoginKey::Esc);
        assert_eq!((f.step(), f.selected()), (LoginStep::Method, 1));
        f.handle(LoginKey::Esc);
        assert_eq!((f.step(), f.selected()), (LoginStep::Provider, 0));
        assert!(matches!(f.handle(LoginKey::Esc), LoginEvent::Cancelled));

        let mut f = LoginFlow::for_provider("anthropic").unwrap();
        assert!(f.note().unwrap().contains("subscription login"));
        assert!(matches!(f.handle(LoginKey::Esc), LoginEvent::Cancelled));
        assert!(LoginFlow::for_provider("gemini").is_none());
    }

    #[test]
    fn empty_and_invalid_values_stay_on_the_value_step() {
        let mut f = LoginFlow::for_provider("openai").unwrap();
        f.click(0);
        assert!(matches!(f.handle(LoginKey::Enter), LoginEvent::None));
        assert_eq!(f.message(), Some("enter the api key"));
        f.handle(LoginKey::Esc);
        f.click(1);
        typed(&mut f, "pass show x ; rm -rf");
        assert!(matches!(f.handle(LoginKey::Enter), LoginEvent::None));
        assert_eq!(f.step(), LoginStep::Value);
        assert!(f.message().unwrap().contains("comment"));
    }

    #[test]
    fn openai_offers_sign_in_with_chatgpt_by_keyboard_and_mouse() {
        // Keyboard: Down to the fourth method, Enter submits at once.
        let mut f = LoginFlow::for_provider("openai").unwrap();
        let labels: Vec<String> = f.options().into_iter().map(|o| o.label).collect();
        assert_eq!(labels.last().unwrap(), "Sign in with ChatGPT");
        assert_eq!(labels.len(), 4);
        for _ in 0..3 {
            f.handle(LoginKey::Down);
        }
        let LoginEvent::Submit(req) = f.handle(LoginKey::Enter) else {
            panic!("expected submit")
        };
        assert_eq!(req.provider, "openai");
        assert!(matches!(req.credential, Credential::ChatGpt));
        assert_eq!(f.step(), LoginStep::Verifying);
        assert!(f.title().contains("in your browser"), "{}", f.title());
        f.finish(Err("ChatGPT sign-in was declined in the browser".into()));
        assert_eq!(f.step(), LoginStep::Failed);
        // Enter retries the sign-in directly.
        assert!(matches!(f.handle(LoginKey::Enter), LoginEvent::Submit(_)));
        f.finish(Ok("signed in".into()));
        assert_eq!(f.step(), LoginStep::Done);

        // Mouse: a click on the option.
        let mut f = LoginFlow::new();
        f.click(1);
        assert!(matches!(f.click(3), LoginEvent::Submit(_)));
        assert_eq!(f.method(), Some(Method::ChatGpt));
        assert!(matches!(f.handle(LoginKey::Esc), LoginEvent::Cancelled));

        // Other providers do not offer it.
        let f = LoginFlow::for_provider("anthropic").unwrap();
        assert_eq!(f.options().len(), 3);
        let mut f = LoginFlow::for_provider("openrouter").unwrap();
        assert!(matches!(f.click(3), LoginEvent::None));
    }

    #[test]
    fn masked_input_is_private() {
        let mut m = MaskedInput::new();
        m.push_str("abc\u{1b}\r\n");
        assert_eq!(m.len(), 3);
        assert_eq!(m.mask(), "•••");
        assert_eq!(format!("{m:?}"), "MaskedInput(3 chars)");
        assert_eq!(m.take().expose(), "abc");
        assert!(m.is_empty());
    }

    #[test]
    fn auth_support_lists_every_provider() {
        for id in crate::ini::config::PROVIDER_IDS {
            assert!(auth_support(id).is_some(), "{id}");
        }
        assert_eq!(auth_support("anthropic").unwrap().methods, ["API key"]);
        assert!(
            auth_support("openai")
                .unwrap()
                .methods
                .contains(&"Sign in with ChatGPT")
        );
    }
}
