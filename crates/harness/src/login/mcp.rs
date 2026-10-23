//! `harness login mcp <name>` / `harness logout mcp <name>`, and
//! [`authorize`]: the OAuth browser flow for a remote MCP server, shared
//! with the TUI `/login mcp <name>` action.
//!
//! Tokens go to the protected MCP token store
//! (`<data_dir>/auth/mcp/<name>.json`, mode `0600`), never to harness.ini;
//! a successful authorization sets `auth = oauth` in `[mcp.<name>]`.

use std::io::Write;
use std::path::Path;

use harness_config::ini::edit;
use harness_config::ini::{McpAuth, McpTransport};
use harness_mcp::auth::{Authorize, Browser};

use super::{Prompt, TerminalBrowser, config_at};

/// Usage of `harness login mcp` and `harness logout mcp`.
pub const USAGE: &str = "\
usage: harness login mcp <name>
       harness logout mcp <name> [--yes]

login mcp authorizes harness with the remote MCP server [mcp.<name>] using
OAuth (the MCP authorization spec): it finds the server's authorization
server, registers harness when the server allows it (or uses
oauth_client_id), opens the browser to approve access, stores the tokens in
<data_dir>/auth/mcp (mode 0600, never in harness.ini) and sets auth = oauth
in the section. Tokens are refreshed automatically. Runs never open a
browser: a server that needs authorization fails with a message naming this
command. logout mcp deletes the stored tokens and registration after
confirmation (--yes skips it).";

/// Authorizes `[mcp.<server>]` from the file at `path` in `browser`, then
/// sets `auth = oauth`. Returns a one-line summary or an actionable error;
/// neither contains a token.
pub async fn authorize(path: &Path, server: &str, browser: &dyn Browser) -> Result<String, String> {
    let cfg = config_at(path)?;
    let Some(s) = cfg.mcp.get(server) else {
        return Err(format!(
            "there is no [mcp.{server}] section in {}; add one with url = <the server's MCP endpoint> first",
            path.display()
        ));
    };
    if !matches!(s.transport, McpTransport::Http { .. }) {
        return Err(format!(
            "[mcp.{server}] is a stdio server; OAuth is only for remote servers (url). Give a stdio server its credentials with env.*"
        ));
    }
    if s.headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("authorization"))
    {
        return Err(format!(
            "[mcp.{server}] sets header.Authorization; remove it before using OAuth (auth = oauth sends its own Authorization header)"
        ));
    }
    let store = crate::wiring::mcp_token_store(&cfg);
    let done = Authorize::new(server, s, store.clone())
        .map_err(|e| e.to_string())?
        .run(browser)
        .await
        .map_err(|e| e.to_string())?;
    let mut set = String::new();
    if s.auth != McpAuth::OAuth {
        edit::save_section_key(path, &format!("mcp.{server}"), "auth", "oauth")
            .map_err(|e| e.to_string())?;
        set = format!("; set auth = oauth in {}", path.display());
    }
    let scopes = if done.scopes.is_empty() {
        String::new()
    } else {
        format!(", scopes: {}", done.scopes.join(" "))
    };
    let refresh = if done.refreshable {
        "refreshed automatically"
    } else {
        "no refresh token was issued, so run this again when it expires"
    };
    Ok(format!(
        "authorized MCP server {server} with {} ({}{scopes}){set}; tokens are in {} (mode 0600), {refresh}",
        done.issuer,
        done.registration.label(),
        store.path(server).display()
    ))
}

/// `harness login mcp <name>`. Returns the exit code.
pub async fn login_main(
    args: &[String],
    path: &Path,
    browser: &dyn Browser,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let server = match args {
        [name] if !name.starts_with('-') => name.as_str(),
        _ => {
            let _ = writeln!(
                err,
                "harness login mcp: expected one server name\n\n{USAGE}"
            );
            return 2;
        }
    };
    let (tx, mut urls) = tokio::sync::mpsc::unbounded_channel();
    let shown = TerminalBrowser {
        shown: tx,
        inner: browser,
    };
    let running = authorize(path, server, &shown);
    tokio::pin!(running);
    let result = loop {
        tokio::select! {
            r = &mut running => break r,
            Some(url) = urls.recv() => {
                let _ = writeln!(
                    err,
                    "Opening your browser to authorize MCP server {server}. If it does not open, visit:\n  {url}\nWaiting for the browser…"
                );
            }
        }
    };
    match result {
        Ok(summary) => {
            let _ = writeln!(out, "{summary}");
            0
        }
        Err(e) => {
            let _ = writeln!(err, "harness login mcp: {e}");
            1
        }
    }
}

/// `harness logout mcp <name> [--yes]`. Returns the exit code.
pub fn logout_main(
    args: &[String],
    path: &Path,
    prompt: &mut dyn Prompt,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let yes = args.iter().any(|a| a == "--yes" || a == "-y");
    let rest: Vec<&String> = args
        .iter()
        .filter(|a| *a != "--yes" && *a != "-y")
        .collect();
    let server = match rest.as_slice() {
        [name] if !name.starts_with('-') => name.as_str(),
        _ => {
            let _ = writeln!(
                err,
                "harness logout mcp: expected one server name\n\n{USAGE}"
            );
            return 2;
        }
    };
    let cfg = match config_at(path) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(err, "harness logout mcp: {e}");
            return 1;
        }
    };
    let store = crate::wiring::mcp_token_store(&cfg);
    if !store.path(server).exists() {
        let _ = writeln!(out, "no stored authorization for MCP server {server}");
        return 0;
    }
    if !yes {
        if !prompt.interactive() {
            let _ = writeln!(
                err,
                "harness logout mcp: stdin is not a terminal; pass --yes to confirm"
            );
            return 2;
        }
        let q = format!(
            "Delete the OAuth tokens and registration for MCP server {server} in {}? [y/N] ",
            store.dir().display()
        );
        match prompt.line(&q) {
            Ok(Some(a)) if matches!(a.to_ascii_lowercase().as_str(), "y" | "yes") => {}
            Ok(_) => {
                let _ = writeln!(err, "harness logout mcp: cancelled; nothing was changed");
                return 1;
            }
            Err(e) => {
                let _ = writeln!(err, "harness logout mcp: cannot read input: {e}");
                return 1;
            }
        }
    }
    match store.delete(server) {
        Ok(_) => {
            let _ = writeln!(
                out,
                "deleted the OAuth tokens for MCP server {server}; run `harness login mcp {server}` to authorize again"
            );
            0
        }
        Err(e) => {
            let _ = writeln!(err, "harness logout mcp: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_mcp::auth::mock::{MockBrowser, MockConfig, MockOAuthServer};

    struct Yes;
    impl Prompt for Yes {
        fn interactive(&self) -> bool {
            true
        }
        fn secret(&mut self, _: &str) -> std::io::Result<Option<harness_config::secrets::Secret>> {
            Ok(None)
        }
        fn line(&mut self, _: &str) -> std::io::Result<Option<String>> {
            Ok(Some("y".into()))
        }
    }

    struct Setup {
        mock: MockOAuthServer,
        _tmp: tempfile::TempDir,
        path: std::path::PathBuf,
        data: std::path::PathBuf,
    }

    /// A hermetic harness.ini (data dir in a temp dir) with `[mcp.docs]`.
    async fn setup(config: MockConfig, extra: &str) -> Setup {
        let mock = MockOAuthServer::start(config).await;
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let path = tmp.path().join("harness.ini");
        std::fs::write(
            &path,
            format!(
                "[general]\ndata_dir = {}\n\n[mcp.docs]\nurl = {}\n{extra}",
                data.display(),
                mock.mcp_url()
            ),
        )
        .unwrap();
        Setup {
            mock,
            _tmp: tmp,
            path,
            data,
        }
    }

    async fn login(s: &Setup, args: &[&str]) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = login_main(&args, &s.path, &MockBrowser::new(), &mut out, &mut err).await;
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[tokio::test]
    async fn login_mcp_authorizes_stores_tokens_and_sets_auth() {
        let s = setup(MockConfig::default(), "").await;
        let (code, out, err) = login(&s, &["docs"]).await;
        assert_eq!(code, 0, "{err}");
        assert!(
            err.contains("Opening your browser to authorize MCP server docs"),
            "{err}"
        );
        assert!(out.contains("authorized MCP server docs"), "{out}");
        assert!(out.contains("dynamic client registration"), "{out}");
        assert!(out.contains("set auth = oauth"), "{out}");
        assert!(out.contains("refreshed automatically"), "{out}");
        let ini = std::fs::read_to_string(&s.path).unwrap();
        assert!(ini.contains("auth = oauth"), "{ini}");
        let cfg = harness_config::ini::load_file(&s.path).unwrap();
        assert!(cfg.mcp["docs"].uses_oauth());

        // Tokens live in the protected data dir, never in harness.ini.
        let store = crate::wiring::mcp_token_store(&cfg);
        assert!(store.dir().starts_with(s.data.join("auth")));
        let data = harness_config::paths::DataDir::from_config(&cfg);
        assert!(
            data.protected_paths(None)
                .iter()
                .any(|p| store.dir().starts_with(p) && p == &data.auth())
        );
        let secrets = store.secrets();
        assert!(!secrets.is_empty());
        for t in &secrets {
            assert!(!ini.contains(t.as_str()));
            assert!(!out.contains(t.as_str()) && !err.contains(t.as_str()));
        }
        // The session redactor knows them.
        let creds = crate::wiring::credentials(&cfg);
        assert!(secrets.iter().all(|t| creds.contains(t)));
        let r = crate::wiring::redactor(&creds);
        assert!(
            !r.redact(&format!("x {}", secrets[0]))
                .contains(secrets[0].as_str())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path("docs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        // logout deletes them.
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = logout_main(&["docs".into()], &s.path, &mut Yes, &mut out, &mut err);
        assert_eq!(code, 0);
        assert!(!store.path("docs").exists());
        let _ = &s.mock;
    }

    #[tokio::test]
    async fn refused_logins_change_nothing() {
        let s = setup(
            MockConfig {
                deny: true,
                ..MockConfig::default()
            },
            "",
        )
        .await;
        let (code, _, err) = login(&s, &["docs"]).await;
        assert_eq!(code, 1);
        assert!(err.contains("declined"), "{err}");
        assert!(!std::fs::read_to_string(&s.path).unwrap().contains("auth ="));

        let (code, _, err) = login(&s, &["nope"]).await;
        assert_eq!(code, 1);
        assert!(err.contains("no [mcp.nope] section"), "{err}");
        let (code, _, err) = login(&s, &[]).await;
        assert_eq!(code, 2);
        assert!(err.contains("usage: harness login mcp"), "{err}");

        let s = setup(MockConfig::default(), "header.Authorization = Bearer x\n").await;
        let (code, _, err) = login(&s, &["docs"]).await;
        assert_eq!(code, 1);
        assert!(err.contains("remove it before using OAuth"), "{err}");
        assert!(s.mock.requests_to("/authorize").is_empty());
    }

    #[tokio::test]
    async fn logout_needs_confirmation_without_a_terminal() {
        let s = setup(MockConfig::default(), "").await;
        assert_eq!(login(&s, &["docs"]).await.0, 0);
        struct NoTty;
        impl Prompt for NoTty {
            fn interactive(&self) -> bool {
                false
            }
            fn secret(
                &mut self,
                _: &str,
            ) -> std::io::Result<Option<harness_config::secrets::Secret>> {
                Ok(None)
            }
            fn line(&mut self, _: &str) -> std::io::Result<Option<String>> {
                Ok(None)
            }
        }
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            logout_main(&["docs".into()], &s.path, &mut NoTty, &mut out, &mut err),
            2
        );
        let cfg = harness_config::ini::load_file(&s.path).unwrap();
        assert!(crate::wiring::mcp_token_store(&cfg).has_tokens("docs"));
        let code = logout_main(
            &["docs".into(), "--yes".into()],
            &s.path,
            &mut NoTty,
            &mut out,
            &mut err,
        );
        assert_eq!(code, 0);
        assert!(!crate::wiring::mcp_token_store(&cfg).has_tokens("docs"));
    }
}
