//! Browser sign-ins from the TUI: Sign in with ChatGPT (the `/login`
//! modal) and MCP OAuth (`/login mcp <name>`).
//!
//! Each sign-in runs as a background task, so the TUI stays responsive
//! while the browser is open. When the flow has its link, the sign-in modal
//! opens with it ([`CoreEvent::SignIn`]); a local browser is opened only
//! when one can reach this machine ([`Environment::opens_browser`]).
//! Addresses pasted in the modal go to the flow, which checks them and
//! answers ([`CoreEvent::SignInAnswer`]). Cancel aborts the task, which
//! closes its loopback listener. The outcome comes back to the driver
//! ([`Finished`]) and then to the UI ([`CoreEvent::SignInDone`]). Codes and
//! tokens never reach the UI or events.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use harness_providers::chatgpt::Browser;
use harness_providers::signin::{
    DeviceCode, Environment, Instructions, Paste, PasteReceiver, PasteSender, paste_channel,
};
use harness_tui::app::{AppEvent, CoreEvent};
use harness_tui::signin::{CHATGPT_ID, PastedText, SignInPrompt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::Driver;

/// A running sign-in.
struct Running {
    paste: PasteSender,
    task: JoinHandle<()>,
}

/// A sign-in that ended.
pub(super) struct Finished {
    /// Which one.
    pub(super) id: String,
    /// A one-line summary or an error.
    pub(super) result: Result<String, String>,
}

/// The driver's running sign-ins.
pub(super) struct SignIns {
    /// Where harness runs (tests replace it).
    pub(super) env: Environment,
    running: HashMap<String, Running>,
    done_tx: mpsc::UnboundedSender<Finished>,
    done_rx: Option<mpsc::UnboundedReceiver<Finished>>,
}

impl Default for SignIns {
    fn default() -> Self {
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        Self {
            env: Environment::detect(),
            running: HashMap::new(),
            done_tx,
            done_rx: Some(done_rx),
        }
    }
}

impl SignIns {
    /// Where finished sign-ins arrive (taken once by the driver loop).
    pub(super) fn take_done(&mut self) -> mpsc::UnboundedReceiver<Finished> {
        self.done_rx.take().expect("taken once")
    }

    /// Stops every running sign-in.
    pub(super) fn abort_all(&mut self) {
        for (_, r) in self.running.drain() {
            r.task.abort();
        }
    }
}

/// The browser of a TUI sign-in: opens the sign-in modal with the link,
/// then the system browser unless the browser is elsewhere.
pub(super) struct ModalBrowser {
    tx: mpsc::Sender<AppEvent>,
    inner: Arc<dyn Browser>,
    env: Environment,
    id: String,
    title: String,
}

impl Browser for ModalBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        let shown = Instructions::new(url, &self.env);
        let ev = AppEvent::Core(CoreEvent::SignIn(SignInPrompt {
            id: self.id.clone(),
            title: self.title.clone(),
            url: url.to_owned(),
            forward: shown.forward,
            no_browser: shown.no_browser.map(str::to_owned),
            device: None,
        }));
        self.show(ev, url);
        Ok(())
    }

    fn show_device_code(&self, code: &DeviceCode) -> Result<(), String> {
        let ev = AppEvent::Core(CoreEvent::SignIn(SignInPrompt {
            id: self.id.clone(),
            title: self.title.clone(),
            url: code.link().to_owned(),
            // Nothing comes back to this machine: no port to forward.
            forward: None,
            no_browser: self.env.reason().map(str::to_owned),
            device: Some(code.clone()),
        }));
        self.show(ev, code.link());
        Ok(())
    }
}

impl ModalBrowser {
    /// Opens the modal with `ev`, and `url` in the system browser when it
    /// can reach this machine.
    fn show(&self, ev: AppEvent, url: &str) {
        // Waits for room in a busy channel rather than losing the link.
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(ev).await;
        });
        if self.env.opens_browser()
            && let Err(e) = self.inner.open(url)
        {
            tracing::warn!(error = %e, "could not open a browser");
        }
    }
}

/// Starts sign-in `id` in the background (replacing a running one with the
/// same id): `run` gets the browser and the pasted addresses.
pub(super) fn start<F, Fut>(d: &mut Driver, id: &str, title: String, run: F)
where
    F: FnOnce(ModalBrowser, PasteReceiver) -> Fut,
    Fut: Future<Output = Result<String, String>> + Send + 'static,
{
    cancel(d, id);
    let browser = ModalBrowser {
        tx: d.tx.clone(),
        inner: d.browser.clone(),
        env: d.sign_ins.env.clone(),
        id: id.to_owned(),
        title,
    };
    let (paste, pasted) = paste_channel();
    let running = run(browser, pasted);
    let (done, owned) = (d.sign_ins.done_tx.clone(), id.to_owned());
    let task = tokio::spawn(async move {
        let result = running.await;
        let _ = done.send(Finished { id: owned, result });
    });
    d.sign_ins
        .running
        .insert(id.to_owned(), Running { paste, task });
}

/// Sign in with ChatGPT for the `/login` modal's submission.
pub(super) fn start_chatgpt(d: &mut Driver, sub: &harness_tui::login::LoginSubmission) {
    let (path, verifier, sub) = (d.login_path.clone(), d.login_verifier.clone(), sub.clone());
    start(
        d,
        CHATGPT_ID,
        "Sign in with ChatGPT".into(),
        move |browser, mut paste| async move {
            let env = |k: &str| std::env::var(k).ok();
            crate::login::apply_with(
                &path,
                sub.request(),
                verifier.as_ref(),
                &env,
                &browser,
                &mut paste,
            )
            .await
        },
    );
}

/// Hands `text` to running sign-in `id`; its answer goes to the UI.
pub(super) async fn paste(d: &mut Driver, id: &str, text: &PastedText) {
    let Some(r) = d.sign_ins.running.get(id) else {
        d.send(CoreEvent::SignInAnswer {
            id: id.to_owned(),
            result: Err("this sign-in is no longer running; start it again".into()),
        })
        .await;
        return;
    };
    let (p, reply) = Paste::new(text.text());
    if r.paste.send(p).is_err() {
        return;
    }
    let (tx, id) = (d.tx.clone(), id.to_owned());
    d.background.push(tokio::spawn(async move {
        // No answer means the flow ended; `Finished` reports that.
        if let Ok(result) = reply.await {
            let _ = tx
                .send(AppEvent::Core(CoreEvent::SignInAnswer { id, result }))
                .await;
        }
    }));
}

/// Stops sign-in `id`; its loopback listener closes with the task.
pub(super) fn cancel(d: &mut Driver, id: &str) {
    if let Some(r) = d.sign_ins.running.remove(id) {
        r.task.abort();
    }
}

/// A sign-in ended: apply the outcome and tell the UI.
pub(super) async fn finished(d: &mut Driver, f: Finished) {
    if d.sign_ins.running.remove(&f.id).is_none() {
        // Cancelled (aborted) or replaced meanwhile.
        return;
    }
    if f.id == CHATGPT_ID {
        super::commands::after_login(d, harness_providers::openai::PROVIDER_NAME, &f.result).await;
        d.send(CoreEvent::LoginResult(f.result.clone())).await;
    } else if let Some(server) = f.id.strip_prefix(super::mcp_login::SIGN_IN_PREFIX) {
        super::mcp_login::done(d, server, &f.result).await;
    }
    d.send(CoreEvent::SignInDone {
        id: f.id,
        result: f.result,
    })
    .await;
}
