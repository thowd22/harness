//! Browser sign-in when the browser is on another machine: shared by Sign in
//! with ChatGPT ([`crate::chatgpt::SignIn`]) and MCP OAuth.
//!
//! Both flows redirect the browser to a loopback listener
//! (`http://127.0.0.1:<port>/…`). Over SSH, or on a machine without a
//! display, the browser runs elsewhere and its redirect never reaches
//! harness. Three ways out, all shown to the user:
//!
//! - **Paste-back**: after signing in, the browser fails to load the
//!   loopback address; the user copies that address (or just the code) and
//!   pastes it into harness. A [`Paste`] goes to the flow through a
//!   [`PasteSender`]; the flow checks `state` (and everything else it checks
//!   on a real callback) and answers whether it took it.
//! - **Port forwarding**: over SSH, [`port_forward_command`] is the
//!   `ssh -L` command that carries the redirect to this machine.
//! - **Device authorization** (RFC 8628), for MCP servers whose
//!   authorization server advertises a `device_authorization_endpoint`:
//!   no loopback redirect at all; the user opens a link on any device and
//!   confirms a short code ([`DeviceCode`], shown through
//!   `Browser::show_device_code`). Sign in with ChatGPT does not offer it:
//!   OpenAI documents only the loopback flow for this client.
//!
//! [`Environment`] decides whether opening a local browser makes sense.
//! Pasted text contains a one-time code; it is held in [`Zeroizing`]
//! strings and never logged.

use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

/// Where harness runs, as far as signing in is concerned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// The SSH connection, when this is an SSH session.
    pub ssh: Option<SshSession>,
    /// Linux or BSD without `$DISPLAY` or `$WAYLAND_DISPLAY`.
    pub headless: bool,
    /// Windows Subsystem for Linux.
    pub wsl: bool,
    /// `$BROWSER` is set: the user chose how to open links.
    pub browser_set: bool,
    /// `$USER`, for the `ssh -L` command.
    pub user: Option<String>,
}

/// The server end of an SSH connection (`$SSH_CONNECTION`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshSession {
    /// The address the client connected to, if `$SSH_CONNECTION` names it.
    pub host: Option<String>,
    /// The SSH port on this machine.
    pub port: u16,
}

impl Environment {
    /// The environment of this process.
    pub fn detect() -> Self {
        Self::from_env(&|k| std::env::var(k).ok())
    }

    /// The environment described by `env` (variable lookup).
    pub fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Self {
        let set = |k: &str| env(k).is_some_and(|v| !v.trim().is_empty());
        let ssh = match env("SSH_CONNECTION").filter(|v| !v.trim().is_empty()) {
            // "client_ip client_port server_ip server_port"
            Some(c) => {
                let parts: Vec<&str> = c.split_whitespace().collect();
                Some(SshSession {
                    host: parts.get(2).map(|s| (*s).to_owned()),
                    port: parts.get(3).and_then(|p| p.parse().ok()).unwrap_or(22),
                })
            }
            None if set("SSH_TTY") || set("SSH_CLIENT") => Some(SshSession {
                host: None,
                port: 22,
            }),
            None => None,
        };
        let unix_desktop = !cfg!(any(target_os = "macos", windows));
        Self {
            ssh,
            headless: unix_desktop && !set("DISPLAY") && !set("WAYLAND_DISPLAY"),
            wsl: set("WSL_DISTRO_NAME") || set("WSL_INTEROP"),
            browser_set: set("BROWSER"),
            user: env("USER").filter(|u| !u.trim().is_empty()),
        }
    }

    /// The browser is probably on another machine (or cannot be started
    /// from here).
    pub fn is_remote(&self) -> bool {
        self.ssh.is_some() || self.headless || self.wsl
    }

    /// Whether to open a browser on this machine: not when remote, unless
    /// `$BROWSER` says how.
    pub fn opens_browser(&self) -> bool {
        self.browser_set || !self.is_remote()
    }

    /// Why no browser is opened, for messages; `None` when one is.
    pub fn reason(&self) -> Option<&'static str> {
        if self.opens_browser() {
            None
        } else if self.ssh.is_some() {
            Some("this is an SSH session")
        } else if self.wsl {
            Some("this is WSL")
        } else {
            Some("there is no display")
        }
    }
}

/// The `ssh -L` command that forwards `port` on the machine with the
/// browser to the same port here. Run it on the browser's machine
/// (OpenSSH, including Windows 10 and later); `-N` only forwards.
pub fn port_forward_command(ssh: &SshSession, user: Option<&str>, port: u16) -> String {
    let host = ssh.host.as_deref().unwrap_or("<this-host>");
    let target = match user {
        Some(u) => format!("{u}@{host}"),
        None => host.to_owned(),
    };
    let p = if ssh.port == 22 {
        String::new()
    } else {
        format!(" -p {}", ssh.port)
    };
    format!("ssh -N -L {port}:127.0.0.1:{port}{p} {target}")
}

/// What to show for one sign-in: the link, and the alternatives that fit
/// this environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instructions {
    /// The authorization URL, in full.
    pub url: String,
    /// The loopback port the browser is sent back to.
    pub port: Option<u16>,
    /// The `ssh -L` command, over SSH.
    pub forward: Option<String>,
    /// Why no local browser is opened (`None`: one is).
    pub no_browser: Option<&'static str>,
}

impl Instructions {
    /// Instructions for `url` in `env`.
    pub fn new(url: &str, env: &Environment) -> Self {
        let port = redirect_port(url);
        let forward = match (&env.ssh, port) {
            (Some(ssh), Some(p)) => Some(port_forward_command(ssh, env.user.as_deref(), p)),
            _ => None,
        };
        Self {
            url: url.to_owned(),
            port,
            forward,
            no_browser: env.reason(),
        }
    }

    /// Plain-text lines for a terminal: the link on a line of its own
    /// (never cut), then how to finish from another machine. `can_paste`:
    /// harness reads pasted addresses (stdin is a terminal).
    pub fn lines(&self, what: &str, can_paste: bool) -> Vec<String> {
        let mut out = Vec::new();
        match self.no_browser {
            Some(why) => out.push(format!(
                "Not opening a browser here ({why}). Open this link in a browser on any machine to {what}:"
            )),
            None => out.push(format!(
                "Opening your browser to {what}. If it does not open, visit:"
            )),
        }
        out.push(self.url.clone());
        if self.no_browser.is_some() {
            out.push(String::new());
            out.push(if can_paste {
                "After you approve, the browser goes to a 127.0.0.1 address that will not load on another machine. Copy that whole address from the address bar and paste it here."
                    .into()
            } else {
                "After you approve, the browser goes to a 127.0.0.1 address that will not load on another machine. To paste that address back, run this command in a terminal."
                    .into()
            });
            if let Some(cmd) = &self.forward {
                out.push(format!(
                    "Or forward the port first, on the machine with the browser, then open the link:\n{cmd}"
                ));
            }
        }
        out
    }
}

/// A device authorization (RFC 8628) to show the user: the code to
/// confirm and where. The user code and the complete link (which carries
/// it) are for the user's eyes only: `Debug` hides them, and they are
/// never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceCode {
    /// The code the user enters or confirms (`user_code`).
    pub user_code: Zeroizing<String>,
    /// The page where the user enters it (`verification_uri`).
    pub verification_uri: String,
    /// The same page with the code filled in
    /// (`verification_uri_complete`), when the server sends one.
    pub verification_uri_complete: Option<Zeroizing<String>>,
    /// Seconds until the code expires (`expires_in`).
    pub expires_in: u64,
}

impl std::fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceCode")
            .field("verification_uri", &self.verification_uri)
            .field("user_code", &"[shown to the user]")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

impl DeviceCode {
    /// The link to open: the complete one when the server sent it, else
    /// the page where the code is typed.
    pub fn link(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .map_or(self.verification_uri.as_str(), String::as_str)
    }

    /// How long the code lasts, for messages (`10 minutes`).
    pub fn lifetime(&self) -> String {
        match self.expires_in {
            s if s >= 120 => format!("{} minutes", s / 60),
            s => format!("{s} seconds"),
        }
    }

    /// Plain-text lines for a terminal: the link and the code each on a
    /// line of their own (never cut), so the terminal's selection copies
    /// them. `no_browser`: why no browser was opened here.
    pub fn lines(&self, what: &str, no_browser: Option<&str>) -> Vec<String> {
        let mut out = vec![match no_browser {
            Some(why) => format!(
                "Not opening a browser here ({why}). To {what}, open this link in a browser on any device:"
            ),
            None => format!(
                "Opening your browser to {what}. If it does not open, visit this link on any device:"
            ),
        }];
        out.push(self.link().to_owned());
        out.push(if self.verification_uri_complete.is_some() {
            "and check that it shows this code:".into()
        } else {
            "and enter this code:".into()
        });
        out.push(self.user_code.to_string());
        if self.verification_uri_complete.is_some() {
            out.push(format!(
                "(Or open {} and type the code.)",
                self.verification_uri
            ));
        }
        out.push(format!(
            "The code expires in {}. Nothing needs to be pasted back here.",
            self.lifetime()
        ));
        out
    }
}

/// The port of the `redirect_uri` in an authorization URL.
pub fn redirect_port(authorize_url: &str) -> Option<u16> {
    let url = reqwest::Url::parse(authorize_url).ok()?;
    let redirect = url
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.into_owned())?;
    reqwest::Url::parse(&redirect).ok()?.port()
}

/// `text` as an OSC 8 hyperlink to `url`. Control characters are dropped
/// from both, so neither can end the sequence early.
pub fn hyperlink(url: &str, text: &str) -> String {
    let clean = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>();
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", clean(url), clean(text))
}

/// Parses a pasted callback: the whole redirect address, its query string,
/// or a bare authorization code (`[("code", …)]`). The result has a `code`
/// or an `error`.
pub fn parse_pasted(text: &str) -> Result<Vec<(String, String)>, String> {
    let t = text
        .trim()
        .trim_matches(|c| matches!(c, '"' | '\'' | '<' | '>'))
        .trim();
    if t.is_empty() {
        return Err("paste the address the browser went to after you approved".into());
    }
    let query = if t.contains("://") {
        let url = reqwest::Url::parse(t)
            .map_err(|_| "that is not a complete address; copy all of it".to_owned())?;
        url.query().unwrap_or("").to_owned()
    } else if let Some(q) = t.strip_prefix('?') {
        q.to_owned()
    } else if t.contains('=') {
        t.split_once('?').map_or(t, |(_, q)| q).to_owned()
    } else if t.len() >= 4
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~+/=%".contains(&b))
    {
        return Ok(vec![("code".into(), t.to_owned())]);
    } else {
        return Err("that does not look like the sign-in address or a code".into());
    };
    let params = crate::chatgpt::parse_query(&query);
    let has = |k: &str| params.iter().any(|(n, v)| n == k && !v.is_empty());
    if !has("code") && !has("error") {
        return Err(
            "that address has no code; copy the address the browser went to after you approved"
                .into(),
        );
    }
    Ok(params)
}

/// The value of `k` in `params`.
pub fn param<'a>(params: &'a [(String, String)], k: &str) -> Option<&'a str> {
    params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

/// Pasted text for a running sign-in, with a way to answer whether it was
/// taken.
#[derive(Debug)]
pub struct Paste {
    text: Zeroizing<String>,
    reply: Option<oneshot::Sender<Result<(), String>>>,
}

impl Paste {
    /// A paste and the receiver of the flow's answer: `Ok` when the flow
    /// took it (the code exchange follows), `Err(why)` when it is wrong and
    /// the flow keeps waiting.
    pub fn new(text: impl Into<String>) -> (Self, oneshot::Receiver<Result<(), String>>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                text: Zeroizing::new(text.into()),
                reply: Some(tx),
            },
            rx,
        )
    }

    fn answer(mut self, r: Result<(), String>) {
        if let Some(tx) = self.reply.take() {
            let _ = tx.send(r);
        }
    }
}

/// Sends pasted callbacks to a running sign-in.
pub type PasteSender = mpsc::UnboundedSender<Paste>;
/// Where a sign-in receives pasted callbacks.
pub type PasteReceiver = mpsc::UnboundedReceiver<Paste>;

/// A channel for pasted callbacks.
pub fn paste_channel() -> (PasteSender, PasteReceiver) {
    mpsc::unbounded_channel()
}

/// Waits for a paste that parses and passes `check`, answering every
/// paste. Never returns once every sender is gone.
pub async fn next_paste(
    rx: &mut PasteReceiver,
    mut check: impl FnMut(&[(String, String)]) -> Result<(), String>,
) -> Vec<(String, String)> {
    loop {
        let Some(paste) = rx.recv().await else {
            return std::future::pending().await;
        };
        match parse_pasted(&paste.text).and_then(|p| check(&p).map(|()| p)) {
            Ok(params) => {
                paste.answer(Ok(()));
                return params;
            }
            Err(e) => paste.answer(Err(e)),
        }
    }
}

/// The paste check for `state`: a pasted address must carry this attempt's
/// state; a bare code has none (PKCE still binds it to this attempt).
pub fn check_state(params: &[(String, String)], state: &str) -> Result<(), String> {
    match param(params, "state") {
        Some(s) if s != state => Err(
            "that address belongs to another sign-in attempt; open the link shown here and paste the address it leads to"
                .into(),
        ),
        Some(_) => Ok(()),
        None if params.len() == 1 => Ok(()),
        None => Err("that address has no state; copy the whole address".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)]) -> Environment {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Environment::from_env(&move |k| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()))
    }

    #[test]
    fn ssh_without_display_does_not_open_a_browser() {
        let e = env(&[
            ("SSH_CONNECTION", "10.0.0.5 51234 192.168.1.20 2222"),
            ("USER", "dev"),
        ]);
        assert!(e.is_remote());
        assert!(!e.opens_browser());
        assert_eq!(e.reason(), Some("this is an SSH session"));
        let ssh = e.ssh.as_ref().unwrap();
        assert_eq!(ssh.host.as_deref(), Some("192.168.1.20"));
        assert_eq!(
            port_forward_command(ssh, e.user.as_deref(), 1455),
            "ssh -N -L 1455:127.0.0.1:1455 -p 2222 dev@192.168.1.20"
        );
        // SSH_TTY alone still counts; the host is a placeholder.
        let t = env(&[("SSH_TTY", "/dev/pts/1"), ("DISPLAY", ":0")]);
        assert!(!t.opens_browser());
        assert_eq!(
            port_forward_command(t.ssh.as_ref().unwrap(), None, 9),
            "ssh -N -L 9:127.0.0.1:9 <this-host>"
        );
    }

    #[test]
    fn desktop_wsl_and_browser_override() {
        let desktop = env(&[("DISPLAY", ":0")]);
        assert!(desktop.opens_browser());
        assert_eq!(desktop.reason(), None);
        let wsl = env(&[("WSL_DISTRO_NAME", "Ubuntu"), ("DISPLAY", ":0")]);
        assert_eq!(wsl.reason(), Some("this is WSL"));
        let chosen = env(&[("SSH_TTY", "/dev/pts/1"), ("BROWSER", "w3m")]);
        assert!(chosen.is_remote() && chosen.opens_browser());
        if !cfg!(any(target_os = "macos", windows)) {
            assert_eq!(env(&[]).reason(), Some("there is no display"));
            assert!(!env(&[("WAYLAND_DISPLAY", "wayland-0")]).headless);
        }
    }

    #[test]
    fn instructions_name_the_port_and_the_forward() {
        let url = "https://auth.example/authorize?client_id=x&redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback&state=s";
        let e = env(&[("SSH_CONNECTION", "1.2.3.4 5 10.1.1.1 22"), ("USER", "me")]);
        let i = Instructions::new(url, &e);
        assert_eq!(i.port, Some(1455));
        assert_eq!(
            i.forward.as_deref(),
            Some("ssh -N -L 1455:127.0.0.1:1455 me@10.1.1.1")
        );
        let lines = i.lines("sign in", true);
        assert_eq!(lines[1], url, "the URL is a line of its own, never cut");
        assert!(lines.iter().any(|l| l.contains("paste it here")));
        assert!(lines.iter().any(|l| l.contains("ssh -N -L 1455")));
        let no_tty = i.lines("sign in", false);
        assert!(!no_tty.iter().any(|l| l.contains("paste it here")));
        assert!(
            no_tty
                .iter()
                .any(|l| l.contains("run this command in a terminal"))
        );
        let local = Instructions::new(url, &env(&[("DISPLAY", ":0")]));
        assert_eq!(local.forward, None);
        assert_eq!(local.lines("sign in", true).len(), 2);
    }

    #[test]
    fn pasted_text_forms() {
        let full =
            parse_pasted("  http://127.0.0.1:1455/auth/callback?code=abc&state=xyz \n").unwrap();
        assert_eq!(param(&full, "code"), Some("abc"));
        assert_eq!(param(&full, "state"), Some("xyz"));
        let q = parse_pasted("?code=a%2Bb&state=s").unwrap();
        assert_eq!(param(&q, "code"), Some("a+b"));
        let bare_q = parse_pasted("/auth/callback?code=c&state=s").unwrap();
        assert_eq!(param(&bare_q, "code"), Some("c"));
        let code = parse_pasted("\"ac_0123456789abcdef\"").unwrap();
        assert_eq!(
            code,
            vec![("code".to_owned(), "ac_0123456789abcdef".to_owned())]
        );
        let denied =
            parse_pasted("http://127.0.0.1:9/callback?error=access_denied&state=s").unwrap();
        assert_eq!(param(&denied, "error"), Some("access_denied"));
        assert!(parse_pasted("").is_err());
        assert!(parse_pasted("hello there").is_err());
        assert!(parse_pasted("http://127.0.0.1:9/callback?state=s").is_err());
        assert!(parse_pasted("abc").is_err());
    }

    #[test]
    fn state_check_allows_bare_codes_only() {
        let ok = vec![("code".into(), "c".into()), ("state".into(), "s".into())];
        assert!(check_state(&ok, "s").is_ok());
        assert!(
            check_state(&ok, "t")
                .unwrap_err()
                .contains("another sign-in attempt")
        );
        assert!(check_state(&[("code".into(), "c".into())], "s").is_ok());
        let no_state = vec![("code".into(), "c".into()), ("iss".into(), "i".into())];
        assert!(check_state(&no_state, "s").is_err());
    }

    #[test]
    fn hyperlinks_cannot_be_broken_out_of() {
        assert_eq!(
            hyperlink("https://a.example/x", "link"),
            "\x1b]8;;https://a.example/x\x1b\\link\x1b]8;;\x1b\\"
        );
        let h = hyperlink("https://a\x07.example/\x1b]0;x", "t\x1bx");
        assert_eq!(h.matches('\x1b').count(), 4);
        assert!(!h.contains('\x07'));
    }

    #[test]
    fn device_codes_show_link_and_code_but_debug_hides_them() {
        let mut d = DeviceCode {
            user_code: Zeroizing::new("WDJB-MJHT".into()),
            verification_uri: "https://auth.example/device".into(),
            verification_uri_complete: Some(Zeroizing::new(
                "https://auth.example/device?user_code=WDJB-MJHT".into(),
            )),
            expires_in: 900,
        };
        assert_eq!(d.link(), "https://auth.example/device?user_code=WDJB-MJHT");
        let lines = d.lines("authorize MCP server docs", Some("this is an SSH session"));
        assert!(lines[0].contains("Not opening a browser here (this is an SSH session)"));
        assert_eq!(lines[1], d.link(), "the link is a line of its own");
        assert_eq!(lines[3], "WDJB-MJHT", "the code is a line of its own");
        assert!(lines[4].contains("https://auth.example/device and type"));
        assert!(lines[5].contains("15 minutes"));
        let debug = format!("{d:?}");
        assert!(!debug.contains("WDJB"), "{debug}");
        d.verification_uri_complete = None;
        d.expires_in = 30;
        assert_eq!(d.link(), "https://auth.example/device");
        let lines = d.lines("x", None);
        assert!(lines[0].starts_with("Opening your browser"));
        assert_eq!(lines[2], "and enter this code:");
        assert!(lines[4].contains("30 seconds"));
    }

    #[tokio::test]
    async fn pastes_are_answered_until_one_passes() {
        let (tx, mut rx) = paste_channel();
        let (bad, bad_reply) = Paste::new("nonsense text");
        let (wrong, wrong_reply) = Paste::new("?code=c&state=other");
        let (good, good_reply) = Paste::new("?code=c&state=s");
        for p in [bad, wrong, good] {
            tx.send(p).unwrap();
        }
        let params = next_paste(&mut rx, |p| check_state(p, "s")).await;
        assert_eq!(param(&params, "code"), Some("c"));
        assert!(bad_reply.await.unwrap().is_err());
        assert!(wrong_reply.await.unwrap().unwrap_err().contains("another"));
        assert_eq!(good_reply.await.unwrap(), Ok(()));
        // With every sender gone it waits forever (the flow's timeout ends it).
        drop(tx);
        let waited = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            next_paste(&mut rx, |_| Ok(())),
        )
        .await;
        assert!(waited.is_err());
    }
}
