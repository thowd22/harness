//! The TUI application and its async event loop.
//!
//! Everything reaches the UI as an [`AppEvent`] on one tokio `mpsc` channel:
//! terminal input ([`forward_terminal_events`]), live run events from
//! `RunRecorder::subscribe()` ([`forward_run_events`]), Unix signals
//! ([`forward_signals`]), messages from the core ([`CoreEvent`]: approval
//! prompts, command output, palette entries) and commands from components
//! ([`AppCommand`]). The UI answers through an unbounded channel of
//! [`UiRequest`]s, so the core never waits on the UI.
//!
//! The [`App`] composes the views:
//!
//! ```text
//! ┌ chat (ChatView) or map (MapView) ──────────────┬ goal bar ┐
//! │  … or a screen on top: node detail, diff,      │ (GoalBar)│
//! │    request inspector, command panel            │          │
//! ├────────────────────────────────────────────────┴──────────┤
//! │ chat  map │ ⠹ running                                keys │  ← footer
//! └───────────────────────────────────────────────────────────┘
//! ```
//!
//! Tab switches between chat and map. Enter or a click on a map node (or a
//! subagent card) opens its detail; Esc backs out of every screen. Clicks go
//! to the component under the pointer; keys go to the open overlay, screen
//! or view. The goal bar is reached with a click or the palette's "Focus
//! goal bar" entry.
//!
//! [`run_loop`] draws only when the [`App`] is dirty, coalesces bursts of
//! events into one frame, and wakes for animation ticks only while something
//! is running, so an idle UI sleeps on the channel and uses no CPU.
//!
//! The terminal is owned by a [`TerminalSession`] guard: raw mode, the
//! alternate screen and mouse capture are undone when it drops, on normal
//! exit, on error and during a panic unwind (the panic hook in
//! [`crate::terminal`] covers aborting panics). Ctrl+Z suspends to the shell
//! and redraws fully on resume.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use futures::StreamExt;
use harness_config::ini::config::Config;
use harness_events::blobs::BlobStore;
use harness_events::reducer::RunState;
use harness_events::schema::{NodeId, RunEvent, RunStatus};
use harness_hygiene::report::HygieneReport;
use harness_policy::approvals::PromptEnvelope;
use harness_policy::rules::Mode;
use harness_providers::catalog::Catalog;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{Frame, Terminal};
use time::{OffsetDateTime, UtcOffset};
use tokio::sync::{broadcast, mpsc};

use crate::chat::view::{ChatConfig, ChatOutput, ChatView};
use crate::clipboard::{Clipboard, ClipboardMode, Copied};
use crate::detail::{DetailOutput, DetailView};
use crate::diff::{DiffOutput, DiffView, FileReverter};
use crate::goalbar::{GoalBar, GoalBarOutput, Goals};
use crate::input::SlashCommand;
use crate::inspector::{Inspection, InspectorOutput, InspectorView};
use crate::interaction::{Dispatch, ElementId, HitMap, Interaction};
use crate::keys::{Action, HelpOverlay, KeyChord, KeyError, Keymap, SUSPEND_CHORD};
use crate::login::{LoginAction, LoginModal, LoginOutcome, LoginSubmission};
use crate::map::{MapConfig, MapOutput, MapView};
use crate::memory::browser::{BrowserAction, BrowserOutcome, MemoryBrowser, MemoryData, SearchRow};
use crate::model_picker::{ModelPicker, PickerAction, PickerOutcome};
use crate::notify::Level;
use crate::palette::{EntryKind, Invocation, PaletteEntry};
use crate::panel::{Panel, PanelButton, PanelOutput};
use crate::selection::{self, Chrome, KeyOutcome, MouseOutcome, Scroll, Selector, TextRegion};
use crate::session_picker::{SessionAction, SessionOutcome, SessionPicker, SessionRow};
use crate::signin::{
    CHATGPT_ID, Hyperlink, PastedText, SignInAction, SignInModal, SignInOutcome, SignInPrompt,
};
use crate::status::StatusLine;
use crate::theme::{ColorDepth, Theme, ThemeError, Token};
use crate::update::{UpdateAction, UpdateModal, UpdateNotice, UpdateOutcome};

/// Spinner frame interval while something runs.
pub const TICK_INTERVAL: Duration = Duration::from_millis(80);

/// Capacity of the [`AppEvent`] channel created by [`event_channel`].
pub const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Maximum number of queued events folded into one frame.
pub const MAX_BATCH: usize = 256;

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const STATIC_SPINNER: char = '◐';

/// Element id of the input line (owned by the chat view).
pub const INPUT_ID: &str = crate::input::INPUT_ID;
/// Element id of the footer's help button.
pub const HELP_BUTTON_ID: &str = "footer.help";
/// Palette entry id that moves keyboard focus into the goal bar.
pub const FOCUS_GOALBAR_ID: &str = "goalbar.focus";
/// Palette entry id that opens the full status line in a panel.
pub const SHOW_STATUS_ID: &str = "app.status";
/// Palette entry id that restores the selected map node's checkpoint.
pub const RESTORE_CHECKPOINT_ID: &str = "map.restore_checkpoint";

/// The main views, cycled by `switch_view`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Conversation.
    Chat,
    /// Agent map.
    Map,
}

impl View {
    /// The other view.
    pub fn next(self) -> View {
        match self {
            View::Chat => View::Map,
            View::Map => View::Chat,
        }
    }

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            View::Chat => "chat",
            View::Map => "map",
        }
    }
}

/// Actions behind the app's own clickable elements (footer, help).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    /// Show a view.
    SwitchView(View),
    /// Open the key help overlay.
    ShowHelp,
    /// Close the key help overlay.
    CloseHelp,
    /// Keyboard selection: copy the selection and leave.
    SelectCopy,
    /// Keyboard selection: leave without copying.
    SelectDone,
}

/// Commands other components (palette, slash commands) send to the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    /// Toggle mouse capture (native text selection when off).
    ToggleMouse,
    /// Set mouse capture.
    SetMouse(bool),
    /// Open the key help overlay.
    ShowHelp,
    /// Show a view.
    SwitchView(View),
    /// Open the full status line in a panel (the status line's `+N more`
    /// button, the "Show status" palette entry).
    ShowStatus,
    /// Quit.
    Quit,
}

/// What the diff screen should show, sent by the core for `/diff` or a
/// node's "diff" action.
pub struct DiffSpec {
    /// Project root (paths in the run log are relative to it).
    pub root: PathBuf,
    /// One node's changes, or the whole session's.
    pub node: Option<NodeId>,
    /// Hygiene classification, when known.
    pub report: Option<HygieneReport>,
    /// Per-file revert (a checkpoint), when available.
    pub reverter: Option<Box<dyn FileReverter>>,
}

impl std::fmt::Debug for DiffSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiffSpec")
            .field("root", &self.root)
            .field("node", &self.node)
            .field("revert", &self.reverter.is_some())
            .finish_non_exhaustive()
    }
}

/// Messages from the core to the UI (besides run events).
#[derive(Debug)]
pub enum CoreEvent {
    /// Session details for the status line.
    Session {
        /// Model alias or reference.
        model: String,
        /// Its context window in tokens.
        context_window: Option<u64>,
        /// Git branch of the project.
        branch: Option<String>,
    },
    /// An approval prompt (tool calls, workflow gates); answer through
    /// its reply channel.
    Approval(PromptEnvelope),
    /// A toast.
    Notify {
        /// Severity.
        level: Level,
        /// Title.
        title: String,
        /// Details.
        body: Option<String>,
    },
    /// Show command output in a panel.
    Panel {
        /// Title.
        title: String,
        /// Text lines.
        lines: Vec<String>,
        /// Buttons; activating one sends [`UiRequest::PanelAction`].
        buttons: Vec<PanelButton>,
    },
    /// Replace the text of the open panel with this title.
    PanelText {
        /// Title of the panel to update.
        title: String,
        /// New text.
        lines: Vec<String>,
    },
    /// Palette entries from the core (commands, workflows, skills,
    /// agents, runs, remotes), shown after the built-in ones.
    PaletteEntries(Vec<PaletteEntry>),
    /// Slash commands offered by input completion.
    SlashCommands(Vec<SlashCommand>),
    /// Ask for an entry's arguments in the palette.
    PromptArgs(PaletteEntry),
    /// Open the diff review screen.
    Diff(DiffSpec),
    /// Hygiene of a node for its detail view.
    Hygiene {
        /// The node.
        node: NodeId,
        /// Change ledger summary lines.
        ledger: Vec<String>,
        /// Hygiene report lines.
        report: Vec<String>,
        /// Temp/debug files found.
        temp_files: usize,
    },
    /// A memory change was undone.
    MemoryUndone(String),
    /// Replace the input draft.
    Draft(String),
    /// A host health badge on the map.
    HostBadge {
        /// Host name.
        host: String,
        /// Badge text.
        badge: String,
        /// Theme token for the badge.
        token: Token,
    },
    /// The effective mode changed (e.g. the agent tightened it).
    Mode(Mode),
    /// The main agent's reasoning effort for the status line
    /// (`effort low`, `no reasoning`; `None` hides it).
    Effort(Option<String>),
    /// Open (or refresh) the `/memory` browser.
    Memory(MemoryData),
    /// Search results for the memory browser.
    MemorySearch(Vec<SearchRow>),
    /// A note's text or a commit's diff for the memory browser.
    MemoryDetail {
        /// The note path or commit id it belongs to.
        key: String,
        /// The text.
        text: String,
    },
    /// The artifacts a node's step wrote, for its detail.
    Artifacts {
        /// The node.
        node: NodeId,
        /// Artifact names.
        names: Vec<String>,
    },
    /// A badge on a map node (budget use); a workflow's badge shows in the
    /// map header.
    NodeBadge {
        /// The node.
        node: NodeId,
        /// Text, e.g. `$0.42/$1.00 42%`.
        badge: String,
        /// Theme token for the badge.
        token: Token,
    },
    /// Open the `/model` picker over everything.
    ModelPicker {
        /// The model catalog (from the cached live lists).
        catalog: Arc<Catalog>,
        /// The session's model (`provider/model`), marked in the list.
        current: Option<String>,
    },
    /// Open the `/resume` picker over these saved sessions (newest
    /// activity first).
    SessionPicker(Vec<SessionRow>),
    /// A newer model catalog (a background refresh started or finished);
    /// replaces the rows of the open picker.
    ModelCatalog(Arc<Catalog>),
    /// Open the `/login` modal, at the provider choice or for one provider.
    Login {
        /// Provider id; `None` asks for it.
        provider: Option<String>,
    },
    /// The outcome of a [`UiRequest::Login`]: a one-line summary or an
    /// actionable error. Never contains the key.
    LoginResult(Result<String, String>),
    /// A browser sign-in started: open the sign-in modal with its link.
    SignIn(SignInPrompt),
    /// The running sign-in's answer to a [`UiRequest::SignInPaste`]:
    /// taken, or why not (the user can paste again).
    SignInAnswer {
        /// The sign-in.
        id: String,
        /// The answer.
        result: Result<(), String>,
    },
    /// A browser sign-in ended: a one-line summary or an error. Never
    /// contains a code or token.
    SignInDone {
        /// The sign-in.
        id: String,
        /// The outcome.
        result: Result<String, String>,
    },
    /// A newer harness release is out: open the update dialog once the
    /// chat input is empty and no turn is running (it closes with Esc or
    /// its close button, answered with [`UiRequest::DismissUpdate`]).
    UpdateAvailable(UpdateNotice),
    /// The MCP servers that failed, by name (empty: none). The status
    /// line shows a compact indicator that opens `/mcp`.
    McpStatus {
        /// Failed server names.
        failed: Vec<String>,
    },
    /// Clear the views for a new session (`/new`, `/resume`).
    Reset,
}

/// Everything the event loop reacts to.
#[derive(Debug)]
pub enum AppEvent {
    /// Terminal input or resize.
    Terminal(Event),
    /// A (redacted) run event from the core.
    Run(Box<RunEvent>),
    /// A command from another component.
    Command(AppCommand),
    /// A message from the core.
    Core(CoreEvent),
    /// The process was continued after a stop (SIGCONT).
    Resumed,
    /// SIGTERM / SIGHUP: quit and restore the terminal.
    Shutdown,
}

/// What the `/memory` browser asks the core for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryRequest {
    /// Search the notes.
    Search(String),
    /// The text of a note (drive-relative path).
    Preview(String),
    /// The diff of a commit.
    Diff(String),
    /// Revert a commit.
    Undo(String),
    /// Turn memory on or off for this session (notes are kept).
    SetEnabled(bool),
    /// Run a dream pass now.
    Dream,
}

/// Requests from the UI to the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiRequest {
    /// The user sent this message (or slash command).
    Submit(String),
    /// Interrupt the running agent (Ctrl+C).
    Interrupt,
    /// Cancel / back with nothing left to back out of (Esc).
    Cancel,
    /// Open the command palette.
    OpenPalette,
    /// A palette entry the UI does not handle (workflow, skill, agent, run,
    /// remote, command).
    Run(Invocation),
    /// The session autonomy mode was changed from the palette.
    ModeChanged(Mode),
    /// Undo a memory change (commit id).
    UndoMemory(String),
    /// A panel button.
    PanelAction(String),
    /// Show the diff of a node (`None`: the session).
    OpenDiff(Option<NodeId>),
    /// Remove a node's temporary files.
    RemoveTempFiles(NodeId),
    /// Restore the project to the checkpoint taken before this node.
    RestoreCheckpoint(NodeId),
    /// List the artifacts of a node (its detail opened).
    Artifacts(NodeId),
    /// Show one artifact's content.
    OpenArtifact {
        /// Node.
        node: NodeId,
        /// Artifact name.
        name: String,
    },
    /// A `/memory` browser action for the core.
    Memory(MemoryRequest),
    /// Switch the session to this model (chosen in the `/model` picker).
    SetModel(String),
    /// Resume this session id (chosen in the `/resume` picker).
    Resume(String),
    /// Verify and save a credential entered in the `/login` modal; answer
    /// with [`CoreEvent::LoginResult`].
    Login(LoginSubmission),
    /// Hand an address pasted in the sign-in modal to the running sign-in;
    /// answer with [`CoreEvent::SignInAnswer`].
    SignInPaste {
        /// The sign-in.
        id: String,
        /// What was pasted.
        text: PastedText,
    },
    /// Stop a running browser sign-in (the user cancelled it).
    SignInCancel(String),
    /// The user closed the update dialog for this version: do not offer
    /// it again.
    DismissUpdate(String),
    /// The user quit.
    Quit,
}

/// Terminal side effects the loop performs for the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Enable or disable mouse capture.
    SetMouseCapture(bool),
    /// Suspend to the shell (Ctrl+Z), then redraw everything.
    Suspend,
    /// Re-enter raw mode after an external stop, then redraw everything.
    Resume,
    /// Write this OSC sequence (terminal title).
    Osc(String),
    /// Copy `text` with the app's [`Clipboard`] and toast the result.
    Copy {
        /// What it is, for the toast (`message`, `diff`, ...).
        what: String,
        /// The text (cleaned and redacted by the clipboard).
        text: String,
    },
    /// Suspend the UI, open this file in `$EDITOR`, then redraw.
    Edit(PathBuf),
}

/// A problem building [`AppConfig`] from `harness.ini`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// `[theme]`.
    Theme(ThemeError),
    /// `[keys]`.
    Keys(KeyError),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Theme(e) => e.fmt(f),
            ConfigError::Keys(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ConfigError {}

/// UI settings resolved from `harness.ini`.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Colours.
    pub theme: Theme,
    /// Key bindings.
    pub keymap: Keymap,
    /// Start with mouse capture on (`[ui] mouse`).
    pub mouse: bool,
    /// Animated spinners (`[ui] animations`).
    pub animations: bool,
    /// Chat settings (theme, keys and animations above win).
    pub chat: ChatConfig,
    /// `[ui] live_preview` on the map.
    pub live_preview: bool,
    /// `[ui] goal_bar`.
    pub goal_bar: harness_config::ini::config::GoalBar,
    /// `[ui] goal_bar_width`.
    pub goal_bar_width: u16,
    /// Project directory (status line).
    pub cwd: PathBuf,
    /// Home directory (shortens paths to `~`).
    pub home: Option<PathBuf>,
    /// Where the request inspector exports calls.
    pub export_dir: PathBuf,
    /// `[ui] clipboard`.
    pub clipboard: ClipboardMode,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            theme: Theme::default(),
            keymap: Keymap::defaults(),
            mouse: true,
            animations: true,
            chat: ChatConfig::default(),
            live_preview: true,
            goal_bar: harness_config::ini::config::GoalBar::Auto,
            goal_bar_width: 32,
            cwd: PathBuf::from("."),
            home: None,
            export_dir: std::env::temp_dir(),
            clipboard: ClipboardMode::Osc52,
        }
    }
}

impl AppConfig {
    /// Resolves the UI settings, detecting colour depth from the environment.
    pub fn from_config(cfg: &Config) -> Result<AppConfig, Vec<ConfigError>> {
        AppConfig::from_config_with(cfg, ColorDepth::detect())
    }

    /// Resolves the UI settings for a known colour depth. All `[theme]` and
    /// `[keys]` problems are reported together.
    pub fn from_config_with(
        cfg: &Config,
        depth: ColorDepth,
    ) -> Result<AppConfig, Vec<ConfigError>> {
        let theme = Theme::from_config_with(cfg, depth);
        let keymap = Keymap::from_config(cfg);
        match (theme, keymap) {
            (Ok(theme), Ok(keymap)) => {
                let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
                Ok(AppConfig {
                    chat: ChatConfig::from_config(cfg, theme.clone(), keymap.clone(), offset),
                    theme,
                    keymap,
                    mouse: cfg.ui.mouse,
                    animations: cfg.ui.animations,
                    live_preview: cfg.ui.live_preview,
                    goal_bar: cfg.ui.goal_bar,
                    goal_bar_width: cfg.ui.goal_bar_width,
                    clipboard: cfg.ui.clipboard,
                    ..AppConfig::default()
                })
            }
            (t, k) => {
                let mut errs: Vec<ConfigError> = t
                    .err()
                    .unwrap_or_default()
                    .into_iter()
                    .map(ConfigError::Theme)
                    .collect();
                errs.extend(
                    k.err()
                        .unwrap_or_default()
                        .into_iter()
                        .map(ConfigError::Keys),
                );
                Err(errs)
            }
        }
    }

    /// With the project and home directories.
    pub fn with_dirs(mut self, cwd: impl Into<PathBuf>, home: Option<PathBuf>) -> AppConfig {
        self.cwd = cwd.into();
        self.home = home;
        self
    }
}

/// Renders the wire body a provider sends for a request
/// (`Provider::request_body`), given the provider's name. Lets the request
/// inspector show the exact payload.
pub type RequestBody = Arc<
    dyn Fn(&str, &harness_providers::model::ChatRequest) -> Option<serde_json::Value> + Send + Sync,
>;

/// A screen shown on top of the chat or map.
enum Screen {
    Detail(Box<DetailView>),
    Diff(Box<DiffView>),
    Inspector(Box<InspectorView>),
    Panel(Box<Panel>),
    Memory(Box<MemoryScreen>),
}

/// The `/login` modal and its hit map.
struct LoginScreen {
    modal: LoginModal,
    interaction: Interaction<LoginAction>,
}

/// The browser sign-in modal and its hit map.
struct SignInScreen {
    modal: SignInModal,
    interaction: Interaction<SignInAction>,
}

/// The update dialog and its hit map.
struct UpdateScreen {
    modal: UpdateModal,
    interaction: Interaction<UpdateAction>,
}

/// The `/model` picker and its hit map.
struct ModelScreen {
    picker: ModelPicker,
    interaction: Interaction<PickerAction>,
}

/// The `/resume` picker and its hit map.
struct SessionScreen {
    picker: SessionPicker,
    interaction: Interaction<SessionAction>,
}

/// The memory browser and its focus model.
struct MemoryScreen {
    browser: MemoryBrowser,
    interaction: Interaction<BrowserAction>,
}

/// The palette entries the app itself handles.
fn app_entries() -> Vec<PaletteEntry> {
    let mut v = vec![
        crate::goalbar::palette_entry(),
        PaletteEntry::new(
            FOCUS_GOALBAR_ID,
            "Focus goal bar",
            "Move the keyboard focus to the goal and task pane",
            EntryKind::Builtin,
        ),
        PaletteEntry::new(
            RESTORE_CHECKPOINT_ID,
            "Restore checkpoint",
            "Restore the files to before the selected map node (or the open node) ran",
            EntryKind::Builtin,
        )
        .with_key("r (map)"),
        PaletteEntry::new(
            SHOW_STATUS_ID,
            "Show status",
            "The full status line: directory, branch, model, usage and indicators",
            EntryKind::Builtin,
        ),
    ];
    v.extend(crate::detail::palette_entries());
    v
}

/// Application state. Pure: it never touches the terminal itself; terminal
/// work is returned as [`Effect`]s.
pub struct App {
    cfg: AppConfig,
    run: RunState,
    events: Vec<RunEvent>,
    view: View,
    chat: ChatView,
    map: MapView,
    screen: Option<Screen>,
    models: Option<Box<ModelScreen>>,
    sessions: Option<Box<SessionScreen>>,
    login: Option<Box<LoginScreen>>,
    sign_in: Option<Box<SignInScreen>>,
    update: Option<Box<UpdateScreen>>,
    /// A newer release the core found, waiting for the chat to be idle.
    pending_update: Option<UpdateNotice>,
    /// Hyperlinks drawn in the last frame, written over its cells.
    links: Vec<Hyperlink>,
    goals: Goals,
    goalbar: GoalBar,
    goal_pane: Option<Rect>,
    goal_focused: bool,
    hygiene: HashMap<NodeId, (Vec<String>, Vec<String>, usize)>,
    artifacts: HashMap<NodeId, Vec<String>>,
    extra_entries: Vec<PaletteEntry>,
    blobs: Option<BlobStore>,
    request_body: Option<RequestBody>,
    help_open: bool,
    /// First row of the help overlay's list.
    help_scroll: u16,
    mouse: bool,
    dirty: bool,
    quit: bool,
    spinner: usize,
    frames: u64,
    ticks: u64,
    footer: Rect,
    interaction: Interaction<UiAction>,
    effects: Vec<Effect>,
    requests: mpsc::UnboundedSender<UiRequest>,
    suspend: KeyChord,
    clipboard: Clipboard,
    select: Selector,
    /// A press on a `/model` or `/resume` picker row, delivered on the
    /// release when no drag (a text selection) happened in between.
    held_press: Option<MouseEvent>,
    clock: Clock,
}

/// The time source for double and triple clicks ([`App::set_clock`]).
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

impl App {
    /// Creates the app. Requests for the core go to `requests`.
    pub fn new(cfg: AppConfig, requests: mpsc::UnboundedSender<UiRequest>) -> App {
        let now = Instant::now();
        let chat_cfg = ChatConfig {
            theme: cfg.theme.clone(),
            keymap: cfg.keymap.clone(),
            animations: cfg.animations,
            ..cfg.chat.clone()
        };
        let status = StatusLine::new(&cfg.cwd, cfg.home.as_deref(), chat_cfg.mode);
        let mut chat = ChatView::new(chat_cfg, status, now);
        chat.set_palette_entries(app_entries());
        let map = MapView::new(MapConfig {
            theme: cfg.theme.clone(),
            animations: cfg.animations,
            live_preview: cfg.live_preview,
        });
        let goalbar = GoalBar::new(
            cfg.theme.clone(),
            cfg.goal_bar,
            cfg.goal_bar_width,
            cfg.animations,
        );
        let cfg_clipboard = cfg.clipboard;
        App {
            mouse: cfg.mouse,
            cfg,
            run: RunState::new(),
            events: Vec::new(),
            view: View::Chat,
            chat,
            map,
            screen: None,
            models: None,
            sessions: None,
            login: None,
            sign_in: None,
            update: None,
            pending_update: None,
            links: Vec::new(),
            goals: Goals::new(),
            goalbar,
            goal_pane: None,
            goal_focused: false,
            hygiene: HashMap::new(),
            artifacts: HashMap::new(),
            extra_entries: Vec::new(),
            blobs: None,
            request_body: None,
            help_open: false,
            help_scroll: 0,
            dirty: true,
            quit: false,
            spinner: 0,
            frames: 0,
            ticks: 0,
            footer: Rect::default(),
            interaction: Interaction::new(),
            effects: Vec::new(),
            requests,
            suspend: KeyChord::parse(SUSPEND_CHORD).expect("suspend chord parses"),
            clipboard: Clipboard::new(cfg_clipboard),
            select: Selector::new(),
            held_press: None,
            clock: Arc::new(Instant::now),
        }
    }

    /// Masks secrets in everything copied with `redactor`.
    pub fn with_redactor(mut self, redactor: Arc<harness_events::redact::Redactor>) -> App {
        self.clipboard = self.clipboard.with_redactor(redactor);
        self
    }

    /// The text selection (mouse and keyboard).
    pub fn selection(&self) -> &Selector {
        &self.select
    }

    /// Replaces the clock that times double and triple clicks (tests).
    pub fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
    }

    /// Replaces the clipboard (tests, custom tools).
    pub fn set_clipboard(&mut self, clipboard: Clipboard) {
        self.clipboard = clipboard;
    }

    /// The clipboard copy actions use.
    pub fn clipboard(&self) -> &Clipboard {
        &self.clipboard
    }

    /// Reports a finished copy ([`Effect::Copy`]) in a toast.
    pub fn copied(&mut self, what: &str, result: Result<Copied, String>) {
        let now = Instant::now();
        match result {
            Ok(c) => self.chat.notify(
                Level::Success,
                &format!("copied {what}"),
                Some(c.summary()),
                now,
            ),
            Err(e) => self
                .chat
                .notify(Level::Error, &format!("copy failed: {what}"), Some(e), now),
        }
        self.dirty = true;
    }

    fn copy_effect(&mut self, what: &str, text: String) {
        self.effects.push(Effect::Copy {
            what: what.to_owned(),
            text,
        });
    }

    /// Resolves request-inspector payloads stored as blobs.
    pub fn with_blobs(mut self, blobs: BlobStore) -> App {
        self.blobs = Some(blobs);
        self
    }

    /// Lets the request inspector render the exact provider body.
    pub fn with_request_body(mut self, f: RequestBody) -> App {
        self.request_body = Some(f);
        self
    }

    /// Whether the next loop iteration must draw.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Forces a redraw.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Whether the app wants to exit.
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Whether animation ticks are needed (something animates and
    /// animations are on).
    pub fn needs_tick(&self) -> bool {
        // A drag held past an edge scrolls on every tick.
        if self.select.autoscroll().is_some() {
            return true;
        }
        if !self.cfg.animations {
            return false;
        }
        let chat = self.view == View::Chat && self.screen.is_none();
        let map = self.view == View::Map && self.screen.is_none();
        self.is_running()
            || self.chat.needs_tick(chat)
            || self.map.needs_tick(map)
            || matches!(&self.screen, Some(Screen::Detail(d)) if d.is_running())
    }

    /// Whether something is working (see [`RunState::is_working`]): an idle
    /// interactive session, including one waiting for its first prompt,
    /// shows "ready", not "running".
    pub fn is_running(&self) -> bool {
        self.run.event_count > 0 && self.run.is_working()
    }

    /// Takes pending terminal effects.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// The folded run state.
    pub fn run_state(&self) -> &RunState {
        &self.run
    }

    /// Every run event received, in order.
    pub fn events(&self) -> &[RunEvent] {
        &self.events
    }

    /// The active view.
    pub fn view(&self) -> View {
        self.view
    }

    /// The chat view.
    pub fn chat(&self) -> &ChatView {
        &self.chat
    }

    /// The chat view, for direct updates.
    pub fn chat_mut(&mut self) -> &mut ChatView {
        &mut self.chat
    }

    /// The map view.
    pub fn map(&self) -> &MapView {
        &self.map
    }

    /// The goals shown in the goal bar.
    pub fn goals(&self) -> &Goals {
        &self.goals
    }

    /// The input draft (pastes expanded).
    pub fn draft(&self) -> String {
        self.chat.input().text()
    }

    /// Name of the open screen (`detail`, `diff`, `inspector`, `panel`).
    pub fn screen(&self) -> Option<&'static str> {
        self.screen.as_ref().map(|s| match s {
            Screen::Detail(_) => "detail",
            Screen::Diff(_) => "diff",
            Screen::Inspector(_) => "inspector",
            Screen::Panel(_) => "panel",
            Screen::Memory(_) => "memory",
        })
    }

    /// The node of the open detail screen.
    pub fn detail_node(&self) -> Option<&NodeId> {
        match &self.screen {
            Some(Screen::Detail(d)) => Some(d.node()),
            _ => None,
        }
    }

    /// The focused element of the open detail screen.
    pub fn detail_focused(&self) -> Option<&ElementId> {
        match &self.screen {
            Some(Screen::Detail(d)) => d.focused(),
            _ => None,
        }
    }

    /// The open panel.
    pub fn panel(&self) -> Option<&Panel> {
        match &self.screen {
            Some(Screen::Panel(p)) => Some(p),
            _ => None,
        }
    }

    /// The open diff screen.
    pub fn diff(&self) -> Option<&DiffView> {
        match &self.screen {
            Some(Screen::Diff(d)) => Some(d),
            _ => None,
        }
    }

    /// Whether the goal bar has keyboard focus.
    pub fn goal_bar_focused(&self) -> bool {
        self.goal_focused
    }

    /// The open `/login` modal.
    pub fn login(&self) -> Option<&LoginModal> {
        self.login.as_ref().map(|l| &l.modal)
    }

    /// The open browser sign-in modal.
    pub fn sign_in(&self) -> Option<&SignInModal> {
        self.sign_in.as_ref().map(|l| &l.modal)
    }

    /// The open update dialog.
    pub fn update_dialog(&self) -> Option<&UpdateModal> {
        self.update.as_ref().map(|u| &u.modal)
    }

    /// The hyperlinks of the last frame ([`Hyperlink::sequence`] draws
    /// them over its cells).
    pub fn hyperlinks(&self) -> &[Hyperlink] {
        &self.links
    }

    /// The open `/model` picker.
    pub fn model_picker(&self) -> Option<&ModelPicker> {
        self.models.as_ref().map(|m| &m.picker)
    }

    /// The open `/resume` picker.
    pub fn session_picker(&self) -> Option<&SessionPicker> {
        self.sessions.as_ref().map(|m| &m.picker)
    }

    /// Whether the help overlay is open.
    pub fn help_open(&self) -> bool {
        self.help_open
    }

    /// Whether mouse capture is on.
    pub fn mouse_enabled(&self) -> bool {
        self.mouse
    }

    /// The focused app element (footer, help).
    pub fn focused(&self) -> Option<&ElementId> {
        self.interaction.focused()
    }

    /// Frames rendered so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Animation ticks handled so far.
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// The UI settings.
    pub fn config(&self) -> &AppConfig {
        &self.cfg
    }

    fn request(&self, r: UiRequest) {
        // The core may already be gone during shutdown; nothing to do then.
        let _ = self.requests.send(r);
    }

    /// Handles one animation tick.
    pub fn tick(&mut self) {
        self.ticks += 1;
        if let Some(s) = self.select.autoscroll() {
            self.wheel(s);
            self.dirty = true;
        }
        if self.needs_tick() {
            self.spinner = (self.spinner + 1) % SPINNER.len();
            let outs = self.chat.tick(Instant::now());
            self.chat_outputs(outs);
            self.map.tick();
            self.goalbar.tick();
            if let Some(Screen::Detail(d)) = &mut self.screen {
                d.tick();
            }
            self.dirty = true;
        }
    }

    /// Handles one event.
    pub fn handle(&mut self, ev: AppEvent) {
        self.chat.set_screen_open(self.screen.is_some());
        self.chat
            .set_covered(self.screen.is_some() || self.view != View::Chat);
        match ev {
            AppEvent::Terminal(Event::Key(k)) => self.handle_key(&k),
            AppEvent::Terminal(Event::Mouse(m)) => self.handle_mouse(&m),
            AppEvent::Terminal(Event::Resize(..)) | AppEvent::Terminal(Event::FocusGained) => {
                self.dirty = true
            }
            AppEvent::Terminal(Event::Paste(text)) => self.handle_paste(text),
            AppEvent::Terminal(Event::FocusLost) => {}
            AppEvent::Run(e) => self.run_event(*e),
            AppEvent::Command(c) => self.command(c),
            AppEvent::Core(c) => self.core_event(c),
            AppEvent::Resumed => {
                self.effects.push(Effect::Resume);
                self.dirty = true;
            }
            AppEvent::Shutdown => self.quit = true,
        }
        self.show_pending_update();
    }

    /// Opens the update dialog the core found, once nothing is typed in the
    /// chat input and no turn is running, so it never takes keys meant for
    /// a prompt.
    fn show_pending_update(&mut self) {
        if self.pending_update.is_none()
            || self.update.is_some()
            || self.is_running()
            || !self.chat.input().is_empty()
        {
            return;
        }
        if let Some(notice) = self.pending_update.take() {
            self.update = Some(Box::new(UpdateScreen {
                modal: UpdateModal::new(notice),
                interaction: Interaction::new(),
            }));
            self.dirty = true;
        }
    }

    /// A bracketed paste: the palette (over any view) or a screen's text
    /// field when one is open, else the chat input (from the map too).
    /// Keys inside the paste never act as keybindings.
    fn handle_paste(&mut self, text: String) {
        if self.help_open {
            return;
        }
        self.dirty = true;
        if let Some(l) = &mut self.sign_in {
            let out = l.modal.paste(&text);
            return self.sign_in_outcome(out);
        }
        if let Some(l) = &mut self.login {
            let out = l.modal.paste(&text);
            return self.login_outcome(out);
        }
        if self.update.is_some() {
            // The dialog has no text field.
            return;
        }
        if let Some(m) = &mut self.models {
            m.picker.paste(&text);
            return;
        }
        if let Some(m) = &mut self.sessions {
            m.picker.paste(&text);
            return;
        }
        let ev = Event::Paste(text);
        if !self.chat.palette().is_open() {
            if let Some(Screen::Memory(m)) = &mut self.screen {
                m.browser.paste(match &ev {
                    Event::Paste(t) => t,
                    _ => unreachable!(),
                });
                return;
            }
            if self.screen.is_some() {
                // Detail, diff, inspector and panels have no text fields.
                return;
            }
            self.goal_focused = false;
            self.view = View::Chat;
        }
        let outs = self
            .chat
            .handle(&ev, Instant::now(), OffsetDateTime::now_utc());
        self.chat_outputs(outs);
    }

    fn run_event(&mut self, e: RunEvent) {
        let now = Instant::now();
        self.run.apply(&e);
        self.chat.apply_run_event(&e, now);
        self.map.apply_run_event(&e);
        self.goals.apply(&e);
        if let Some(Screen::Detail(d)) = &mut self.screen {
            d.apply_run_event(&e);
        }
        self.events.push(e);
        self.dirty = true;
    }

    fn core_event(&mut self, c: CoreEvent) {
        let now = Instant::now();
        match c {
            CoreEvent::Session {
                model,
                context_window,
                branch,
            } => {
                let status = self.chat.status_mut();
                status.set_model(model, context_window);
                status.set_branch(branch);
            }
            CoreEvent::Approval(env) => self.chat.push_approval(env),
            CoreEvent::Notify { level, title, body } => self.chat.notify(level, &title, body, now),
            CoreEvent::Panel {
                title,
                lines,
                buttons,
            } => {
                let panel = Panel::new(title, &lines, self.cfg.theme.clone()).with_buttons(buttons);
                self.screen = Some(Screen::Panel(Box::new(panel)));
            }
            CoreEvent::PanelText { title, lines } => {
                if let Some(Screen::Panel(p)) = &mut self.screen
                    && p.title() == title
                {
                    p.set_text(&lines);
                }
            }
            CoreEvent::PaletteEntries(v) => {
                self.extra_entries = v;
                let mut all = app_entries();
                all.extend(self.extra_entries.iter().cloned());
                self.chat.set_palette_entries(all);
            }
            CoreEvent::SlashCommands(v) => self.chat.set_slash_commands(v),
            CoreEvent::McpStatus { failed } => self.chat.status_mut().set_mcp_failed(failed),
            CoreEvent::PromptArgs(entry) => {
                self.screen = None;
                self.view = View::Chat;
                self.chat.prompt_args(entry);
            }
            CoreEvent::Diff(spec) => {
                let mut view = DiffView::from_events(
                    spec.root,
                    &self.events,
                    spec.node.as_ref(),
                    spec.report.as_ref(),
                    self.cfg.theme.clone(),
                );
                if let Some(r) = spec.reverter {
                    view = view.with_reverter(r);
                }
                self.screen = Some(Screen::Diff(Box::new(view)));
            }
            CoreEvent::Hygiene {
                node,
                ledger,
                report,
                temp_files,
            } => {
                if let Some(Screen::Detail(d)) = &mut self.screen
                    && d.node() == &node
                {
                    d.set_hygiene(ledger.clone(), report.clone(), temp_files);
                }
                self.hygiene.insert(node, (ledger, report, temp_files));
            }
            CoreEvent::MemoryUndone(commit) => self.chat.transcript_mut().memory_undone(&commit),
            CoreEvent::Draft(text) => self.chat.input_mut().set_text(&text),
            CoreEvent::HostBadge { host, badge, token } => {
                self.map.set_host_badge(&host, &badge, token)
            }
            CoreEvent::Mode(m) => self.chat.set_mode(m),
            CoreEvent::Effort(e) => self.chat.status_mut().set_effort(e),
            CoreEvent::Memory(data) => match &mut self.screen {
                Some(Screen::Memory(m)) => m.browser.set_data(data),
                _ => {
                    let mut browser = MemoryBrowser::new(data);
                    let out = browser.open();
                    self.screen = Some(Screen::Memory(Box::new(MemoryScreen {
                        browser,
                        interaction: Interaction::new(),
                    })));
                    self.memory_outcome(out);
                }
            },
            CoreEvent::MemorySearch(rows) => {
                if let Some(Screen::Memory(m)) = &mut self.screen {
                    m.browser.set_search_results(rows);
                }
            }
            CoreEvent::MemoryDetail { key, text } => {
                if let Some(Screen::Memory(m)) = &mut self.screen {
                    m.browser.set_detail(key, text);
                }
            }
            CoreEvent::Artifacts { node, names } => {
                if let Some(Screen::Detail(d)) = &mut self.screen
                    && d.node() == &node
                {
                    d.set_artifacts(names.clone());
                }
                self.artifacts.insert(node, names);
            }
            CoreEvent::NodeBadge { node, badge, token } => {
                self.map.set_node_badge(&node, &badge, token)
            }
            CoreEvent::ModelPicker { catalog, current } => {
                self.models = Some(Box::new(ModelScreen {
                    picker: ModelPicker::new(catalog, current),
                    interaction: Interaction::new(),
                }));
            }
            CoreEvent::SessionPicker(rows) => {
                self.sessions = Some(Box::new(SessionScreen {
                    picker: SessionPicker::new(rows),
                    interaction: Interaction::new(),
                }));
            }
            CoreEvent::Login { provider } => match LoginModal::new(provider.as_deref()) {
                Ok(modal) => {
                    self.login = Some(Box::new(LoginScreen {
                        modal,
                        interaction: Interaction::new(),
                    }))
                }
                Err(e) => self
                    .chat
                    .notify(Level::Error, "login", Some(e), Instant::now()),
            },
            CoreEvent::SignIn(prompt) => {
                self.sign_in = Some(Box::new(SignInScreen {
                    modal: SignInModal::new(prompt),
                    interaction: Interaction::new(),
                }));
            }
            CoreEvent::UpdateAvailable(notice) => self.pending_update = Some(notice),
            CoreEvent::SignInAnswer { id, result } => {
                if let Some(l) = &mut self.sign_in
                    && l.modal.id() == id
                {
                    l.modal.answer(result);
                }
            }
            CoreEvent::SignInDone { id, result } => {
                if let Some(l) = &mut self.sign_in
                    && l.modal.id() == id
                {
                    if id == CHATGPT_ID && self.login.is_some() {
                        // The `/login` modal shows the outcome.
                        self.sign_in = None;
                    } else {
                        l.modal.finish(result);
                    }
                }
            }
            CoreEvent::LoginResult(result) => match &mut self.login {
                Some(l) => l.modal.finish(result),
                // Closed while the key was checked: say how it went.
                None => {
                    let (level, title, body) = match result {
                        Ok(m) => (Level::Success, "logged in", m),
                        Err(e) => (Level::Error, "login failed", e),
                    };
                    self.chat.notify(level, title, Some(body), Instant::now())
                }
            },
            CoreEvent::ModelCatalog(catalog) => {
                if let Some(m) = &mut self.models {
                    m.picker.set_catalog(catalog);
                }
            }
            CoreEvent::Reset => {
                let entries = self.extra_entries.clone();
                let status = self.chat.status_mut().clone();
                let chat_cfg = ChatConfig {
                    theme: self.cfg.theme.clone(),
                    keymap: self.cfg.keymap.clone(),
                    animations: self.cfg.animations,
                    ..self.cfg.chat.clone()
                };
                self.chat = ChatView::new(chat_cfg, status, now);
                let mut all = app_entries();
                all.extend(entries);
                self.chat.set_palette_entries(all);
                self.map = MapView::new(MapConfig {
                    theme: self.cfg.theme.clone(),
                    animations: self.cfg.animations,
                    live_preview: self.cfg.live_preview,
                });
                self.run = RunState::new();
                self.events.clear();
                self.goals = Goals::new();
                self.screen = None;
                self.hygiene.clear();
                self.artifacts.clear();
            }
        }
        self.dirty = true;
    }

    /// Runs a command.
    pub fn command(&mut self, c: AppCommand) {
        match c {
            AppCommand::ToggleMouse => self.set_mouse(!self.mouse),
            AppCommand::SetMouse(on) => self.set_mouse(on),
            AppCommand::ShowHelp => self.activate(UiAction::ShowHelp),
            AppCommand::SwitchView(v) => self.activate(UiAction::SwitchView(v)),
            AppCommand::ShowStatus => self.show_status(),
            AppCommand::Quit => self.do_quit(),
        }
    }

    /// Opens the full status line (directory, branch, model, usage, …) in
    /// a panel: what the one-to-three-row status line could not show.
    fn show_status(&mut self) {
        let mut lines = self.chat.status_details();
        // The goal and tasks, which the collapsed goal bar shows only as
        // progress.
        if let Some(g) = &self.goals.goal {
            lines.push(format!("Goal: {g}"));
        }
        if self.goals.has_tasks() {
            lines.push(format!(
                "Tasks ({}/{} done):",
                self.goals.done(),
                self.goals.total()
            ));
            let spinner = crate::indicators::Spinner::new(false);
            for t in &self.goals.tasks {
                lines.push(format!("  {} {}", t.status.glyph(&spinner).0, t.title));
            }
        }
        let panel = Panel::new("Status", &lines, self.cfg.theme.clone());
        self.screen = Some(Screen::Panel(Box::new(panel)));
    }

    fn set_mouse(&mut self, on: bool) {
        if self.mouse != on {
            self.mouse = on;
            self.effects.push(Effect::SetMouseCapture(on));
            self.dirty = true;
        }
    }

    fn do_quit(&mut self) {
        self.quit = true;
        self.request(UiRequest::Quit);
    }

    fn activate(&mut self, action: UiAction) {
        match action {
            UiAction::SwitchView(v) => {
                self.view = v;
                self.screen = None;
                self.goal_focused = false;
                self.interaction.clear_focus();
            }
            UiAction::ShowHelp => {
                self.help_open = true;
                self.help_scroll = 0;
                self.interaction.set_focus(crate::keys::HELP_CLOSE_ID);
            }
            UiAction::CloseHelp => {
                self.help_open = false;
                self.interaction.clear_focus();
            }
            UiAction::SelectCopy => {
                let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                let out = self.select.key(&enter);
                self.selection_outcome(out);
            }
            UiAction::SelectDone => {
                self.select.clear();
            }
        }
        self.dirty = true;
    }

    fn dispatch(&mut self, d: Dispatch<UiAction>) {
        match d {
            Dispatch::Activate(a) => self.activate(a),
            Dispatch::Back => {
                if self.help_open {
                    self.activate(UiAction::CloseHelp);
                } else {
                    self.interaction.clear_focus();
                }
            }
            Dispatch::Scroll { pane, lines }
                if self.help_open && pane.0 == crate::keys::HELP_PANE =>
            {
                self.scroll_help(lines);
            }
            Dispatch::Scroll { .. } | Dispatch::Focus(_) => self.dirty = true,
        }
    }

    fn scroll_help(&mut self, lines: i32) {
        let next = (i32::from(self.help_scroll) + lines).clamp(0, i32::from(u16::MAX));
        self.help_scroll = next as u16;
        self.dirty = true;
    }

    /// Opens node detail for `node` (and selects it on the map).
    pub fn open_detail(&mut self, node: NodeId) {
        let mut d = DetailView::from_events(
            node.clone(),
            self.cfg.theme.clone(),
            self.cfg.chat.settings,
            self.cfg.animations,
            &self.events,
        );
        if let Some((ledger, report, temp)) = self.hygiene.get(&node) {
            d.set_hygiene(ledger.clone(), report.clone(), *temp);
        }
        if let Some(names) = self.artifacts.get(&node) {
            d.set_artifacts(names.clone());
        }
        self.request(UiRequest::Artifacts(node.clone()));
        self.map.select(node);
        self.screen = Some(Screen::Detail(Box::new(d)));
        self.goal_focused = false;
        self.dirty = true;
    }

    fn chat_outputs(&mut self, outs: Vec<ChatOutput>) {
        for o in outs {
            match o {
                ChatOutput::Submit(text) => self.request(UiRequest::Submit(text)),
                ChatOutput::Interrupt => self.request(UiRequest::Interrupt),
                ChatOutput::Cancel => self.request(UiRequest::Cancel),
                ChatOutput::Quit => self.do_quit(),
                ChatOutput::Command(c) => self.command(c),
                ChatOutput::Run(inv) => self.palette_invocation(inv),
                ChatOutput::OpenNode(n) => self.open_detail(n),
                ChatOutput::ModeChanged(m) => self.request(UiRequest::ModeChanged(m)),
                ChatOutput::SetTitle(osc) => self.effects.push(Effect::Osc(osc)),
                ChatOutput::UndoMemory(c) => self.request(UiRequest::UndoMemory(c)),
                ChatOutput::Copy { what, text } => self.copy_effect(&what, text),
            }
        }
    }

    fn palette_invocation(&mut self, inv: Invocation) {
        if self.goalbar.run_palette(&inv.id) {
            return;
        }
        if inv.id == RESTORE_CHECKPOINT_ID {
            if let Some(node) = self.checkpoint_target() {
                self.request(UiRequest::RestoreCheckpoint(node));
            }
            return;
        }
        if inv.id == crate::chat::copy::ids::SELECT {
            // Only reaches the app while the chat is covered.
            self.start_key_selection();
            return;
        }
        if inv.id == SHOW_STATUS_ID {
            self.show_status();
            return;
        }
        if inv.id == FOCUS_GOALBAR_ID {
            if self.goals.has_tasks() {
                self.goal_focused = true;
                self.goalbar.focus_first();
            }
            return;
        }
        if matches!(
            inv.id.as_str(),
            crate::chat::copy::ids::FOCUSED | crate::chat::copy::ids::DIFF
        ) {
            self.copy_screen();
            return;
        }
        if inv.id.starts_with("detail.") {
            if let Some(Screen::Detail(d)) = &mut self.screen
                && let Some(out) = d.run_palette(&inv)
            {
                self.detail_output(out);
            }
            return;
        }
        self.request(UiRequest::Run(inv));
    }

    /// The node a checkpoint restore applies to: the open detail's node,
    /// else the map selection.
    fn checkpoint_target(&self) -> Option<NodeId> {
        self.detail_node()
            .cloned()
            .or_else(|| self.map.selected().cloned())
    }

    fn detail_output(&mut self, out: DetailOutput) {
        match out {
            DetailOutput::Back => self.screen = None,
            DetailOutput::Copy(text) => self.copy_effect("log entry", text),
            DetailOutput::OpenDiff(node) => self.request(UiRequest::OpenDiff(Some(node))),
            DetailOutput::OpenInspector { call_id, .. } => {
                match Inspection::from_events(&self.events, &call_id, self.blobs.as_ref()) {
                    Some(mut data) => {
                        if let (Some(f), Some(chat)) = (&self.request_body, data.chat.clone())
                            && let Some(body) = f(&data.provider, &chat)
                        {
                            data.set_provider_body(Some(body));
                        }
                        let view = InspectorView::new(
                            data,
                            self.cfg.theme.clone(),
                            self.cfg.export_dir.clone(),
                            None,
                        );
                        self.screen = Some(Screen::Inspector(Box::new(view)));
                    }
                    None => self.chat.notify(
                        Level::Warning,
                        "request not found",
                        Some(format!("no model call `{call_id}` in this run")),
                        Instant::now(),
                    ),
                }
            }
            DetailOutput::RemoveTempFiles(node) => self.request(UiRequest::RemoveTempFiles(node)),
            DetailOutput::OpenArtifact { node, name } => {
                self.request(UiRequest::OpenArtifact { node, name })
            }
        }
    }

    fn memory_outcome(&mut self, out: BrowserOutcome) {
        let req = |r: MemoryRequest| UiRequest::Memory(r);
        match out {
            BrowserOutcome::Ignored | BrowserOutcome::Changed => {}
            BrowserOutcome::Closed => self.screen = None,
            BrowserOutcome::Search(q) => self.request(req(MemoryRequest::Search(q))),
            BrowserOutcome::Preview(p) => self.request(req(MemoryRequest::Preview(p))),
            BrowserOutcome::ShowDiff(c) => self.request(req(MemoryRequest::Diff(c))),
            BrowserOutcome::Undo(c) => self.request(req(MemoryRequest::Undo(c))),
            BrowserOutcome::SetEnabled(on) => self.request(req(MemoryRequest::SetEnabled(on))),
            BrowserOutcome::Dream => self.request(req(MemoryRequest::Dream)),
            BrowserOutcome::OpenInEditor(path) => {
                if let Some(Screen::Memory(m)) = &self.screen {
                    let root = PathBuf::from(&m.browser.data().drive);
                    self.effects.push(Effect::Edit(root.join(path)));
                }
            }
            BrowserOutcome::Draft(text) => {
                self.screen = None;
                self.view = View::Chat;
                self.chat.input_mut().set_text(&text);
            }
        }
    }

    fn memory_event(&mut self, ev: &Event) {
        let Some(Screen::Memory(m)) = &mut self.screen else {
            return;
        };
        let out = match ev {
            Event::Key(k) => {
                let focused = m.interaction.focused().cloned();
                match m.browser.handle_key(k, focused.as_ref()) {
                    BrowserOutcome::Ignored => match m.interaction.handle_key(k) {
                        Some(Dispatch::Activate(a)) => m.browser.activate(a),
                        Some(Dispatch::Back) => m.browser.activate(BrowserAction::Close),
                        _ => BrowserOutcome::Changed,
                    },
                    out => out,
                }
            }
            Event::Mouse(mouse) => match m.interaction.handle_mouse(mouse) {
                Some(Dispatch::Activate(a)) => m.browser.activate(a),
                Some(Dispatch::Back) => m.browser.activate(BrowserAction::Close),
                Some(Dispatch::Scroll { lines, .. }) => {
                    m.browser.scroll_detail(lines);
                    BrowserOutcome::Changed
                }
                _ => BrowserOutcome::Ignored,
            },
            _ => BrowserOutcome::Ignored,
        };
        self.memory_outcome(out);
    }

    /// Input for the open `/login` modal.
    fn login_event(&mut self, ev: &Event) {
        let Some(l) = &mut self.login else {
            return;
        };
        let out = match ev {
            Event::Key(k) => l.modal.handle_key(k),
            Event::Mouse(mouse) => match l.interaction.handle_mouse(mouse) {
                Some(Dispatch::Activate(a)) => l.modal.activate(a),
                Some(Dispatch::Back) => l.modal.activate(LoginAction::Back),
                Some(Dispatch::Scroll { lines, .. }) => l.modal.scroll(lines),
                _ => LoginOutcome::Ignored,
            },
            _ => LoginOutcome::Ignored,
        };
        self.login_outcome(out);
    }

    /// Input for the open sign-in modal.
    fn sign_in_event(&mut self, ev: &Event) {
        let Some(l) = &mut self.sign_in else {
            return;
        };
        let out = match ev {
            Event::Key(k) => l.modal.handle_key(k, &mut l.interaction),
            Event::Mouse(mouse) => match l.interaction.handle_mouse(mouse) {
                Some(Dispatch::Activate(a)) => l.modal.activate(a),
                // A stray click outside must not throw a sign-in away.
                Some(Dispatch::Back) if l.modal.is_finished() => SignInOutcome::Close,
                Some(Dispatch::Scroll { lines, .. }) => l.modal.scroll(lines),
                _ => SignInOutcome::Ignored,
            },
            _ => SignInOutcome::Ignored,
        };
        self.sign_in_outcome(out);
    }

    fn sign_in_outcome(&mut self, out: SignInOutcome) {
        let Some(id) = self.sign_in.as_ref().map(|l| l.modal.id().to_owned()) else {
            return;
        };
        match out {
            SignInOutcome::Copy { what, text } => self.copy_effect(what, text),
            SignInOutcome::Paste(text) => self.request(UiRequest::SignInPaste { id, text }),
            SignInOutcome::Cancel => {
                self.sign_in = None;
                if id == CHATGPT_ID {
                    // The `/login` modal was waiting for this sign-in.
                    self.login = None;
                }
                self.request(UiRequest::SignInCancel(id));
            }
            SignInOutcome::Close => self.sign_in = None,
            SignInOutcome::Ignored | SignInOutcome::Changed => {}
        }
        self.dirty = true;
    }

    /// Input for the open update dialog.
    fn update_event(&mut self, ev: &Event) {
        let Some(u) = &mut self.update else {
            return;
        };
        let out = match ev {
            Event::Key(k) => u.modal.handle_key(k, &mut u.interaction),
            Event::Mouse(m) => u.modal.handle_mouse(m, &mut u.interaction),
            _ => UpdateOutcome::Ignored,
        };
        match out {
            UpdateOutcome::Copy { what, text } => self.copy_effect(what, text),
            UpdateOutcome::Dismiss => {
                let version = u.modal.notice().latest.clone();
                self.update = None;
                self.request(UiRequest::DismissUpdate(version));
            }
            UpdateOutcome::Ignored | UpdateOutcome::Changed => {}
        }
        self.dirty = true;
    }

    fn login_outcome(&mut self, out: LoginOutcome) {
        match out {
            LoginOutcome::Close => self.login = None,
            LoginOutcome::Submit(s) => self.request(UiRequest::Login(s)),
            LoginOutcome::Ignored | LoginOutcome::Changed => {}
        }
        self.dirty = true;
    }

    /// Input for the open `/model` picker.
    fn models_event(&mut self, ev: &Event) {
        let Some(m) = &mut self.models else {
            return;
        };
        let out = match ev {
            Event::Key(k) => m.picker.handle_key(k),
            Event::Mouse(mouse) => match m.interaction.handle_mouse(mouse) {
                Some(Dispatch::Activate(a)) => m.picker.activate(a),
                Some(Dispatch::Scroll { lines, .. }) => m.picker.scroll(lines),
                Some(Dispatch::Back) => PickerOutcome::Close,
                _ => PickerOutcome::Ignored,
            },
            _ => PickerOutcome::Ignored,
        };
        match out {
            PickerOutcome::Close => self.models = None,
            PickerOutcome::Switch(model) => {
                self.models = None;
                self.request(UiRequest::SetModel(model));
            }
            PickerOutcome::Ignored | PickerOutcome::Changed => {}
        }
        self.dirty = true;
    }

    /// Input for the open `/resume` picker.
    fn sessions_event(&mut self, ev: &Event) {
        let Some(m) = &mut self.sessions else {
            return;
        };
        let out = match ev {
            Event::Key(k) => m.picker.handle_key(k),
            Event::Mouse(mouse) => match m.interaction.handle_mouse(mouse) {
                Some(Dispatch::Activate(a)) => m.picker.activate(a),
                Some(Dispatch::Scroll { lines, .. }) => m.picker.scroll(lines),
                Some(Dispatch::Back) => SessionOutcome::Close,
                _ => SessionOutcome::Ignored,
            },
            _ => SessionOutcome::Ignored,
        };
        match out {
            SessionOutcome::Close => self.sessions = None,
            SessionOutcome::Resume(id) => {
                self.sessions = None;
                self.request(UiRequest::Resume(id));
            }
            SessionOutcome::Ignored | SessionOutcome::Changed => {}
        }
        self.dirty = true;
    }

    fn screen_event(&mut self, ev: &Event) {
        if matches!(self.screen, Some(Screen::Memory(_))) {
            return self.memory_event(ev);
        }
        let now = Instant::now();
        let Some(screen) = &mut self.screen else {
            return;
        };
        match screen {
            Screen::Detail(d) => {
                if let Some(out) = d.handle(ev) {
                    self.detail_output(out);
                }
            }
            Screen::Diff(d) => match d.handle(ev) {
                Some(DiffOutput::Back) => self.screen = None,
                Some(DiffOutput::Reverted(path)) => {
                    self.chat
                        .notify(Level::Success, "reverted", Some(path), now)
                }
                Some(DiffOutput::RevertFailed { path, error }) => self.chat.notify(
                    Level::Error,
                    "revert failed",
                    Some(format!("{path}: {error}")),
                    now,
                ),
                Some(DiffOutput::Copy { path, diff }) => {
                    self.copy_effect(&format!("diff of {path}"), diff)
                }
                None => {}
            },
            Screen::Inspector(i) => match i.handle(ev) {
                Some(InspectorOutput::Back) => self.screen = None,
                Some(InspectorOutput::Exported(p)) => self.chat.notify(
                    Level::Success,
                    "request exported",
                    Some(p.display().to_string()),
                    now,
                ),
                Some(InspectorOutput::ExportFailed(e)) => {
                    self.chat
                        .notify(Level::Error, "export failed", Some(e), now)
                }
                None => {}
            },
            Screen::Panel(p) => match p.handle(ev) {
                Some(PanelOutput::Close) => self.screen = None,
                Some(PanelOutput::Action(id)) => self.request(UiRequest::PanelAction(id)),
                None => {}
            },
            Screen::Memory(_) => {}
        }
    }

    /// Copies from the open detail or diff screen. Returns false when no
    /// such screen is open.
    fn copy_screen(&mut self) -> bool {
        let now = Instant::now();
        match &self.screen {
            Some(Screen::Detail(d)) => match d.copy() {
                Some(DetailOutput::Copy(text)) => self.copy_effect("log entry", text),
                _ => self.chat.notify(
                    Level::Warning,
                    "nothing to copy",
                    Some("focus a log entry".into()),
                    now,
                ),
            },
            Some(Screen::Diff(d)) => match d.copy() {
                Some(DiffOutput::Copy { path, diff }) => {
                    self.copy_effect(&format!("diff of {path}"), diff)
                }
                _ => self
                    .chat
                    .notify(Level::Warning, "nothing to copy", None, now),
            },
            _ => return false,
        }
        true
    }

    fn map_output(&mut self, out: MapOutput) {
        match out {
            MapOutput::OpenDetail(n) => self.open_detail(n),
            // Approvals and budget prompts are answered in the chat.
            MapOutput::OpenPrompt { .. } => self.activate(UiAction::SwitchView(View::Chat)),
            MapOutput::Back => self.activate(UiAction::SwitchView(View::Chat)),
        }
    }

    fn goalbar_event(&mut self, ev: &Event) {
        match self.goalbar.handle(ev) {
            Some(GoalBarOutput::OpenNode(n)) => self.open_detail(n),
            Some(GoalBarOutput::Back) => self.goal_focused = false,
            None => {}
        }
    }

    fn handle_key(&mut self, k: &KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        self.dirty = true;
        if self.suspend.matches(k) {
            self.effects.push(Effect::Suspend);
            return;
        }
        if self.help_open {
            if let Some(lines) = crate::keys::help_scroll(k) {
                return self.scroll_help(lines);
            }
            if let Some(d) = self.interaction.handle_key(k) {
                self.dispatch(d);
            }
            return;
        }
        let action = self.cfg.keymap.lookup(k);
        match action {
            Some(Action::Quit) => return self.do_quit(),
            Some(Action::Interrupt) => return self.request(UiRequest::Interrupt),
            _ => {}
        }
        if self.select.keyboard() {
            let out = if action == Some(Action::Copy) {
                self.select
                    .key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            } else {
                self.select.key(k)
            };
            return self.selection_outcome(out);
        }
        if self.select.selection().is_some() {
            // Esc clears a mouse selection; the copy key copies it again;
            // any other key clears it and goes on as usual.
            if k.code == KeyCode::Esc && k.modifiers.is_empty() {
                self.select.clear();
                return;
            }
            let text = self.select.text();
            if action == Some(Action::Copy) && !text.is_empty() {
                return self.copy_effect("selection", text);
            }
            self.select.clear();
        }
        let covered = self.view != View::Chat || self.screen.is_some();
        // Over the pickers too: a press on a picker row picks it, so the
        // keyboard is how to copy a row's text.
        let picker = self.models.is_some() || self.sessions.is_some();
        let overlay = self.login.is_some()
            || self.sign_in.is_some()
            || self.update.is_some()
            || self.chat.palette().is_open()
            || self.goal_focused;
        if action == Some(Action::Select) && (covered || picker) && !overlay {
            return self.start_key_selection();
        }
        if self.sign_in.is_some() {
            return self.sign_in_event(&Event::Key(*k));
        }
        if self.login.is_some() {
            return self.login_event(&Event::Key(*k));
        }
        if self.update.is_some() {
            return self.update_event(&Event::Key(*k));
        }
        if self.models.is_some() {
            return self.models_event(&Event::Key(*k));
        }
        if self.sessions.is_some() {
            return self.sessions_event(&Event::Key(*k));
        }
        // The palette (in the chat) works on top of every view and screen.
        let palette = action == Some(Action::Palette) || self.chat.palette().is_open();
        if palette {
            if !self.chat.palette().is_open() {
                self.refresh_live_run_entry();
            }
            self.view_or_chat_key(k, true);
            return;
        }
        if self.goal_focused {
            return self.goalbar_event(&Event::Key(*k));
        }
        if action == Some(Action::Copy) && self.copy_screen() {
            return;
        }
        if self.screen.is_some() {
            return self.screen_event(&Event::Key(*k));
        }
        match self.view {
            View::Chat => self.view_or_chat_key(k, false),
            View::Map => {
                if action == Some(Action::SwitchView) {
                    self.activate(UiAction::SwitchView(View::Chat));
                } else if k.code == crossterm::event::KeyCode::Char('r') && k.modifiers.is_empty() {
                    if let Some(node) = self.checkpoint_target() {
                        self.request(UiRequest::RestoreCheckpoint(node));
                    }
                } else if let Some(out) = self.map.handle(&Event::Key(*k)) {
                    self.map_output(out);
                }
            }
        }
    }

    /// Rebuilds the palette entry of the run in progress from the live
    /// state, so it shows the same title and elapsed time as the map rather
    /// than the snapshot the core listed when the session started.
    fn refresh_live_run_entry(&mut self) {
        if self.run.run_id.is_none() {
            return;
        }
        let now = OffsetDateTime::now_utc();
        let Some(live) = crate::history::RunSummary::from_state(&self.run, now) else {
            return;
        };
        let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
        crate::palette::refresh_run_entry(&mut self.extra_entries, &live, offset, now);
        let mut all = app_entries();
        all.extend(self.extra_entries.iter().cloned());
        self.chat.set_palette_entries(all);
    }

    /// Sends a key to the chat. With `overlay` the chat's palette is shown
    /// over whatever view is active.
    fn view_or_chat_key(&mut self, k: &KeyEvent, overlay: bool) {
        let outs = self
            .chat
            .handle(&Event::Key(*k), Instant::now(), OffsetDateTime::now_utc());
        // A view switch from the chat means "the other view".
        let outs = outs
            .into_iter()
            .map(|o| match o {
                ChatOutput::Command(AppCommand::SwitchView(_)) if !overlay => {
                    ChatOutput::Command(AppCommand::SwitchView(self.view.next()))
                }
                o => o,
            })
            .collect();
        self.chat_outputs(outs);
    }

    /// Starts keyboard selection in the view or screen that is shown.
    fn start_key_selection(&mut self) {
        if !self.select.start_keyboard() {
            self.chat
                .notify(Level::Warning, "nothing to select", None, Instant::now());
        }
        self.dirty = true;
    }

    fn selection_outcome(&mut self, out: KeyOutcome) {
        match out {
            KeyOutcome::Ignored | KeyOutcome::Changed | KeyOutcome::Exit => {}
            KeyOutcome::Scroll(s) => self.wheel(s),
            KeyOutcome::Copy(text) if text.is_empty() => {
                self.chat
                    .notify(Level::Warning, "nothing to copy", None, Instant::now())
            }
            KeyOutcome::Copy(text) => self.copy_effect("selection", text),
        }
        self.dirty = true;
    }

    /// One wheel notch at `s` (autoscroll while selecting).
    fn wheel(&mut self, s: Scroll) {
        let kind = if s.up {
            MouseEventKind::ScrollUp
        } else {
            MouseEventKind::ScrollDown
        };
        self.route_mouse(&MouseEvent {
            kind,
            column: s.x,
            row: s.y,
            modifiers: KeyModifiers::NONE,
        });
    }

    /// Mouse input: the selection layer sees it first (drags select,
    /// releases copy); everything else is routed as a click or a wheel.
    fn handle_mouse(&mut self, m: &MouseEvent) {
        if !self.mouse {
            return;
        }
        self.dirty = true;
        let footer = self.footer;
        let on_footer = m.column >= footer.x
            && m.column < footer.right()
            && m.row >= footer.y
            && m.row < footer.bottom();
        if self.select.keyboard() && on_footer {
            // The selection bar's buttons.
            return self.route_mouse(m);
        }
        let (outcome, scroll) = self.select.mouse(m, (self.clock)());
        if let Some(s) = scroll {
            self.wheel(s);
        }
        let press = m.kind == MouseEventKind::Down(crossterm::event::MouseButton::Left);
        let release = m.kind == MouseEventKind::Up(crossterm::event::MouseButton::Left);
        if press {
            self.held_press = None;
        }
        match outcome {
            MouseOutcome::Pass if press && self.picker_row_at(m.column, m.row) => {
                // Picking a model or resuming a session is a click: it
                // waits for the release, so a drag selects the row's text.
                self.held_press = Some(*m);
            }
            MouseOutcome::Pass => {
                if release && let Some(p) = self.held_press.take() {
                    self.route_mouse(&p);
                }
                self.route_mouse(m)
            }
            MouseOutcome::Consumed | MouseOutcome::Copy(_) if release => {
                self.held_press = None;
                if let MouseOutcome::Copy(text) = outcome {
                    self.copy_effect("selection", text);
                }
            }
            MouseOutcome::Consumed => {}
            MouseOutcome::Copy(text) => self.copy_effect("selection", text),
        }
    }

    /// Whether `(x, y)` is on a clickable element of the open `/model` or
    /// `/resume` picker.
    fn picker_row_at(&self, x: u16, y: u16) -> bool {
        if self.help_open || self.login.is_some() || self.sign_in.is_some() || self.update.is_some()
        {
            return false;
        }
        if let Some(m) = &self.models {
            return m.interaction.hits().hit(x, y).is_some();
        }
        self.sessions
            .as_ref()
            .is_some_and(|m| m.interaction.hits().hit(x, y).is_some())
    }

    fn route_mouse(&mut self, m: &MouseEvent) {
        if self.sign_in.is_some() && !self.help_open {
            return self.sign_in_event(&Event::Mouse(*m));
        }
        if self.login.is_some() && !self.help_open {
            return self.login_event(&Event::Mouse(*m));
        }
        if self.update.is_some() && !self.help_open {
            return self.update_event(&Event::Mouse(*m));
        }
        if self.models.is_some() && !self.help_open {
            return self.models_event(&Event::Mouse(*m));
        }
        if self.sessions.is_some() && !self.help_open {
            return self.sessions_event(&Event::Mouse(*m));
        }
        let (x, y) = (m.column, m.row);
        let inside = |r: Rect| x >= r.x && x < r.right() && y >= r.y && y < r.bottom();
        if self.help_open || inside(self.footer) {
            if let Some(d) = self.interaction.handle_mouse(m) {
                self.dispatch(d);
            }
            return;
        }
        let ev = Event::Mouse(*m);
        let covered = self.view != View::Chat || self.screen.is_some();
        if covered
            && m.kind == crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
            && !self.chat.palette().is_open()
            && self.chat.click_overlay_toast(x, y)
        {
            return;
        }
        if self.chat.palette().is_open() {
            let outs = self
                .chat
                .handle(&ev, Instant::now(), OffsetDateTime::now_utc());
            return self.chat_outputs(outs);
        }
        if let Some(pane) = self.goal_pane
            && inside(pane)
        {
            self.goal_focused = true;
            return self.goalbar_event(&ev);
        }
        self.goal_focused = false;
        if self.screen.is_some() {
            return self.screen_event(&ev);
        }
        match self.view {
            View::Chat => {
                let outs = self
                    .chat
                    .handle(&ev, Instant::now(), OffsetDateTime::now_utc());
                self.chat_outputs(outs);
            }
            View::Map => {
                if let Some(out) = self.map.handle(&ev) {
                    self.map_output(out);
                }
            }
        }
    }

    fn spinner_char(&self) -> char {
        if self.cfg.animations {
            SPINNER[self.spinner]
        } else {
            STATIC_SPINNER
        }
    }

    /// Renders a frame and registers interactive regions. Clears the dirty flag.
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        self.frames += 1;
        self.dirty = false;
        let theme = self.cfg.theme.clone();
        let area = frame.area();
        frame.render_widget(Block::new().style(theme.base()), area);
        let now = Instant::now();
        let wall = OffsetDateTime::now_utc();

        // The footer wraps onto more rows when its items do not fit.
        let footer_h = self.footer_height(area.width).min(area.height / 3).max(1);
        let [body, footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(footer_h)]).areas(area);
        self.footer = footer;
        let (main, pane) = self.goalbar.split(body, &self.goals);
        self.goal_pane = pane;
        let indicator = self.goalbar.indicator(&self.goals, area.width);
        self.chat.status_mut().set_goal_indicator(indicator);

        selection::begin_frame();
        // Fallback text regions: anything a view does not register itself
        // (headers, plain text) is still selectable; panes, modals and
        // masks registered while rendering lie on top.
        selection::register(TextRegion::new("screen", main).chrome(Chrome::Drawing));
        match &mut self.screen {
            Some(Screen::Detail(d)) => d.render(frame, main, wall),
            Some(Screen::Diff(d)) => d.render(frame, main),
            Some(Screen::Inspector(i)) => i.render(frame, main),
            Some(Screen::Panel(p)) => p.render(frame, main),
            Some(Screen::Memory(m)) => {
                let mut hits = m.interaction.begin_frame();
                m.browser
                    .render(frame, main, &theme, &mut hits, &|a: BrowserAction| a);
                m.interaction.end_frame(hits);
            }
            None => match self.view {
                View::Chat => self.chat.render(frame, main, now),
                View::Map => self.map.render(frame, main, wall),
            },
        }
        // The palette lives in the chat; show it over the map and screens.
        let covered = self.view != View::Chat || self.screen.is_some();
        if self.chat.palette().is_open() && covered {
            self.chat.render(frame, main, now);
        } else if covered {
            // Toasts (copy results, warnings) stay visible everywhere,
            // below the screens' header row and its buttons.
            let below = Rect {
                y: main.y + 1,
                height: main.height.saturating_sub(1),
                ..main
            };
            self.chat.render_toast_overlay(frame, below);
        }
        if let Some(p) = pane {
            selection::register(TextRegion::new("goalbar", p).chrome(Chrome::Drawing));
            self.goalbar.render(frame, p, &self.goals, &self.run);
        }

        let mut hits = self.interaction.begin_frame();
        self.render_footer(frame, footer, &theme, &mut hits);
        // The model picker covers everything but the help overlay.
        if let Some(m) = &mut self.models {
            let mut picker_hits = m.interaction.begin_frame();
            m.picker
                .render(frame, area, &theme, &mut picker_hits, &|a: PickerAction| a);
            m.interaction.end_frame(picker_hits);
        }
        if let Some(m) = &mut self.sessions {
            let mut picker_hits = m.interaction.begin_frame();
            m.picker
                .render(frame, area, &theme, &mut picker_hits, &|a: SessionAction| a);
            m.interaction.end_frame(picker_hits);
        }
        // The update dialog goes over the pickers, under the sign-in
        // modals.
        self.links.clear();
        if let Some(u) = &mut self.update {
            let mut hits = u.interaction.begin_frame();
            u.modal.render(
                frame,
                area,
                &theme,
                &mut hits,
                &|a: UpdateAction| a,
                &mut self.links,
            );
            if u.interaction.end_frame(hits) {
                self.dirty = true;
            }
        }
        // The login modal goes over the picker.
        if let Some(l) = &mut self.login {
            let mut login_hits = l.interaction.begin_frame();
            l.modal
                .render(frame, area, &theme, &mut login_hits, &|a: LoginAction| a);
            l.interaction.end_frame(login_hits);
        }
        // The sign-in modal goes over the `/login` modal that started it.
        if self.login.is_some() || self.sign_in.is_some() {
            // The update dialog's links are covered.
            self.links.clear();
        }
        if let Some(l) = &mut self.sign_in {
            let mut hits = l.interaction.begin_frame();
            l.modal.render(
                frame,
                area,
                &theme,
                &mut hits,
                &|a: SignInAction| a,
                &mut self.links,
            );
            if l.interaction.end_frame(hits) {
                self.dirty = true;
            }
        }
        // Over a modal only the newest toast shows (e.g. "copied selection"
        // after a drag inside a picker), at the bottom right where it hides
        // the modal's help line rather than the rows being worked on.
        if self.models.is_some()
            || self.sessions.is_some()
            || self.login.is_some()
            || self.sign_in.is_some()
            || self.update.is_some()
        {
            let h = 4.min(area.height);
            let at = Rect {
                y: area.bottom() - h,
                height: h,
                ..area
            };
            self.chat.render_toast_overlay(frame, at);
        }
        if self.help_open {
            let max = HelpOverlay::new(&self.cfg.keymap, &theme)
                .scrolled(self.help_scroll)
                .render(frame, area, &mut hits, UiAction::CloseHelp);
            self.help_scroll = self.help_scroll.min(max);
        }
        if self.interaction.end_frame(hits) {
            self.dirty = true;
        }
        let regions = selection::end_frame();
        let style = theme.selection();
        self.select
            .end_frame(regions, frame.buffer_mut(), style, style);
    }

    /// The footer's items: view tabs, the open screen, the run state, the
    /// mouse state and the `keys` button (last). `focused` says whether an
    /// element has the keyboard focus. In keyboard selection mode it is the
    /// selection bar instead:
    /// `SELECT [copy] [done] ←↑↓→ move · shift extend · … · esc done`.
    fn footer_items(&self, theme: &Theme, focused: &dyn Fn(&ElementId) -> bool) -> Vec<FooterItem> {
        let mut items = Vec::new();
        if self.select.keyboard() {
            items.push(FooterItem::text(
                " SELECT ",
                theme.fg_on(Token::Bg, Token::Accent),
            ));
            for (id, label, action) in [
                (SELECT_COPY_ID, "[copy]", UiAction::SelectCopy),
                (SELECT_DONE_ID, "[done]", UiAction::SelectDone),
            ] {
                let eid = ElementId::from(id);
                let mut st = theme.fg(Token::Accent);
                if focused(&eid) {
                    st = st.patch(theme.focused());
                }
                items.push(FooterItem::button(format!(" {label}"), 1, st, eid, action));
            }
            let hints = [
                "←↑↓→ move",
                "shift extend",
                "tab next pane",
                "enter copy",
                "esc done",
            ];
            for (i, h) in hints.iter().enumerate() {
                let sep = if i == 0 { "  " } else { " · " };
                items.push(FooterItem::text(
                    format!("{sep}{h}"),
                    theme.fg(Token::Muted),
                ));
            }
            return items;
        }
        for v in [View::Chat, View::Map] {
            let id = ElementId(format!("footer.view.{}", v.name()));
            let mut style = if v == self.view && self.screen.is_none() {
                theme.fg(Token::Accent).add_modifier(Modifier::BOLD)
            } else {
                theme.fg(Token::Muted)
            };
            if focused(&id) {
                style = style.patch(theme.focused());
            }
            items.push(FooterItem::button(
                format!(" {} ", v.name()),
                0,
                style,
                id,
                UiAction::SwitchView(v),
            ));
        }
        if let Some(s) = self.screen() {
            items.push(FooterItem::text(format!(" › {s}"), theme.fg(Token::Accent)));
        }
        let state = if self.is_running() {
            (format!("{} running", self.spinner_char()), Token::Running)
        } else if self.run.event_count > 0 {
            let (icon, token) = match self.run.status {
                RunStatus::Succeeded => ("✔", Token::Ok),
                RunStatus::Failed => ("✗", Token::Error),
                RunStatus::Running => ("●", Token::Ok),
                RunStatus::Cancelled => ("⊘", Token::Muted),
            };
            let label = match self.run.status {
                RunStatus::Running => "ready".to_owned(),
                s => format!("{s:?}").to_lowercase(),
            };
            (format!("{icon} {label}"), token)
        } else {
            ("idle".to_owned(), Token::Muted)
        };
        items.push(FooterItem {
            spans: vec![
                Span::styled(" │ ", theme.fg(Token::Border)),
                Span::styled(state.0, theme.fg(state.1)),
            ],
            button: None,
        });
        if !self.mouse {
            items.push(FooterItem::text(" │ mouse off", theme.fg(Token::Warn)));
        }
        let id = ElementId::from(HELP_BUTTON_ID);
        let style = if focused(&id) {
            theme.focused().patch(theme.fg(Token::Accent))
        } else {
            theme.fg(Token::Accent)
        };
        items.push(FooterItem::button(
            " keys ",
            0,
            style,
            id,
            UiAction::ShowHelp,
        ));
        items
    }

    /// Rows the footer needs at `width` (it wraps instead of cutting).
    fn footer_height(&self, width: u16) -> u16 {
        let items = self.footer_items(&self.cfg.theme, &|_| false);
        pack_footer(&items, width, !self.select.keyboard())
            .len()
            .max(1) as u16
    }

    fn render_footer(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<UiAction>,
    ) {
        let items = self.footer_items(theme, &|id| hits.is_focused(id));
        let rows = pack_footer(&items, area.width, !self.select.keyboard());
        for (r, row) in rows.iter().enumerate().take(usize::from(area.height)) {
            let y = area.y + r as u16;
            for &(x, i) in row {
                let item = &items[i];
                let rect = Rect::new(area.x + x, y, item.width(), 1).intersection(area);
                if rect.is_empty() {
                    continue;
                }
                frame.render_widget(Paragraph::new(Line::from(item.spans.clone())), rect);
                if let Some((id, action, skip)) = &item.button {
                    let b = Rect {
                        x: rect.x + skip,
                        width: rect.width.saturating_sub(*skip),
                        ..rect
                    };
                    if !b.is_empty() {
                        hits.register(id.clone(), b, action.clone());
                    }
                }
            }
        }
    }
}

/// One piece of the footer: text, maybe a button (its id, action and the
/// leading cells that are not part of it).
struct FooterItem {
    spans: Vec<Span<'static>>,
    button: Option<(ElementId, UiAction, u16)>,
}

impl FooterItem {
    fn text(text: impl Into<String>, style: Style) -> FooterItem {
        FooterItem {
            spans: vec![Span::styled(text.into(), style)],
            button: None,
        }
    }

    fn button(
        text: impl Into<String>,
        skip: u16,
        style: Style,
        id: ElementId,
        action: UiAction,
    ) -> FooterItem {
        let text: String = text.into();
        let (lead, label) = text.split_at(usize::from(skip));
        FooterItem {
            spans: vec![
                Span::raw(lead.to_owned()),
                Span::styled(label.to_owned(), style),
            ],
            button: Some((id, action, skip)),
        }
    }

    fn width(&self) -> u16 {
        self.spans
            .iter()
            .map(|s| crate::chat::text::width(&s.content) as u16)
            .sum()
    }
}

/// Places footer items on rows of `width`: in order, moving to the next row
/// when one does not fit (nothing is cut). With `last_right`, the last item
/// (the `keys` button) is right-aligned on the last row. Returns each row's
/// `(column, item index)` pairs.
fn pack_footer(items: &[FooterItem], width: u16, last_right: bool) -> Vec<Vec<(u16, usize)>> {
    let mut rows: Vec<Vec<(u16, usize)>> = vec![Vec::new()];
    let mut x = 0u16;
    let n = items.len();
    for (i, item) in items.iter().enumerate() {
        let w = item.width();
        let right = last_right && i + 1 == n;
        // The right-aligned button keeps a gap from the text before it.
        let need = if right && x > 0 { w + 1 } else { w };
        if x > 0 && x.saturating_add(need) > width {
            rows.push(Vec::new());
            x = 0;
        }
        let at = if right {
            width.saturating_sub(w).max(x)
        } else {
            x
        };
        rows.last_mut().expect("a row").push((at, i));
        x = at.saturating_add(w);
    }
    rows
}

/// Element id of the keyboard selection bar's copy button.
pub const SELECT_COPY_ID: &str = "footer.select.copy";
/// Element id of the keyboard selection bar's done button.
pub const SELECT_DONE_ID: &str = "footer.select.done";

/// Raw-mode and screen control, abstracted so the loop can be tested.
pub trait TerminalControl {
    /// Enters raw mode and the alternate screen; enables bracketed paste and,
    /// when `mouse`, mouse capture.
    fn enter(&mut self, mouse: bool) -> io::Result<()>;
    /// Undoes [`TerminalControl::enter`]. Must be safe to call repeatedly.
    fn leave(&mut self) -> io::Result<()>;
    /// Turns mouse capture on or off.
    fn set_mouse_capture(&mut self, on: bool) -> io::Result<()>;
    /// Restores the terminal, stops the process (SIGTSTP) and re-enters when
    /// it is continued.
    fn suspend(&mut self) -> io::Result<()>;
    /// Writes an escape sequence (terminal title, clipboard). Does nothing
    /// by default.
    fn write_osc(&mut self, _osc: &str) -> io::Result<()> {
        Ok(())
    }
    /// Leaves the terminal, runs `$VISUAL` / `$EDITOR` (default `vi`) on
    /// `path`, and enters again.
    fn edit(&mut self, path: &std::path::Path) -> io::Result<()> {
        self.leave()?;
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "vi".into());
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("harness-edit")
            .arg(path)
            .status();
        self.enter(true)?;
        status.map(|_| ())
    }
}

/// [`TerminalControl`] for the real terminal via crossterm.
#[derive(Debug, Default)]
pub struct CrosstermControl {
    mouse: bool,
}

impl TerminalControl for CrosstermControl {
    fn enter(&mut self, mouse: bool) -> io::Result<()> {
        use crossterm::event::{EnableBracketedPaste, EnableMouseCapture};
        use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
        self.mouse = mouse;
        enable_raw_mode()?;
        let mut out = io::stdout();
        crossterm::execute!(out, EnterAlternateScreen, EnableBracketedPaste)?;
        if mouse {
            crossterm::execute!(out, EnableMouseCapture)?;
        }
        Ok(())
    }

    fn leave(&mut self) -> io::Result<()> {
        crate::terminal::restore_terminal()
    }

    fn set_mouse_capture(&mut self, on: bool) -> io::Result<()> {
        use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
        self.mouse = on;
        if on {
            crossterm::execute!(io::stdout(), EnableMouseCapture)
        } else {
            crossterm::execute!(io::stdout(), DisableMouseCapture)
        }
    }

    fn write_osc(&mut self, osc: &str) -> io::Result<()> {
        use std::io::Write;
        let mut out = io::stdout();
        out.write_all(osc.as_bytes())?;
        out.flush()
    }

    fn suspend(&mut self) -> io::Result<()> {
        use nix::sys::signal::{Signal, raise};
        self.leave()?;
        // Stops here until the shell continues us (`fg`).
        raise(Signal::SIGTSTP).map_err(io::Error::from)?;
        self.enter(self.mouse)
    }
}

/// RAII owner of the terminal: [`TerminalControl::leave`] runs on drop, so
/// the terminal is restored on every exit path, including unwinding panics.
pub struct TerminalSession<C: TerminalControl> {
    control: C,
    active: bool,
}

impl<C: TerminalControl> TerminalSession<C> {
    /// Enters the terminal. On failure the terminal is restored before the
    /// error is returned.
    pub fn enter(mut control: C, mouse: bool) -> io::Result<Self> {
        if let Err(e) = control.enter(mouse) {
            let _ = control.leave();
            return Err(e);
        }
        Ok(TerminalSession {
            control,
            active: true,
        })
    }

    /// The control, for the event loop.
    pub fn control_mut(&mut self) -> &mut C {
        &mut self.control
    }

    /// Restores the terminal now and reports errors.
    pub fn finish(mut self) -> io::Result<()> {
        self.active = false;
        self.control.leave()
    }
}

impl<C: TerminalControl> Drop for TerminalSession<C> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.control.leave();
        }
    }
}

fn draw_err<E: std::error::Error + Send + Sync + 'static>(e: E) -> io::Error {
    io::Error::other(e)
}

/// Draws if dirty and performs pending effects.
fn flush<B, C>(terminal: &mut Terminal<B>, control: &mut C, app: &mut App) -> io::Result<()>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    C: TerminalControl,
{
    for effect in app.take_effects() {
        match effect {
            Effect::SetMouseCapture(on) => control.set_mouse_capture(on)?,
            Effect::Suspend => {
                control.suspend()?;
                terminal.clear().map_err(draw_err)?;
                app.mark_dirty();
            }
            Effect::Resume => {
                control.enter(app.mouse_enabled())?;
                terminal.clear().map_err(draw_err)?;
                app.mark_dirty();
            }
            Effect::Osc(osc) => control.write_osc(&osc)?,
            Effect::Copy { what, text } => {
                let r = app.clipboard().copy(&text, &mut |s| control.write_osc(s));
                app.copied(&what, r);
            }
            Effect::Edit(path) => {
                // An editor that cannot start is not fatal for the UI.
                if let Err(e) = control.edit(&path) {
                    tracing::warn!("editor: {e}");
                }
                terminal.clear().map_err(draw_err)?;
                app.mark_dirty();
            }
        }
    }
    if app.is_dirty() && !app.should_quit() {
        terminal.draw(|f| app.render(f)).map_err(draw_err)?;
        // Links go over the cells just drawn (ratatui cells cannot hold
        // OSC 8).
        for link in app.hyperlinks() {
            control.write_osc(&link.sequence())?;
        }
    }
    Ok(())
}

/// Runs the UI until the app quits or `events` closes. Draws only when the
/// app is dirty; ticks at [`TICK_INTERVAL`] only while [`App::needs_tick`].
pub async fn run_loop<B, C>(
    terminal: &mut Terminal<B>,
    control: &mut C,
    app: &mut App,
    events: &mut mpsc::Receiver<AppEvent>,
) -> io::Result<()>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    C: TerminalControl,
{
    let mut ticker = tokio::time::interval(TICK_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        flush(terminal, control, app)?;
        if app.should_quit() {
            return Ok(());
        }
        tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { return Ok(()) };
                app.handle(ev);
                // Coalesce a burst into one frame.
                for _ in 0..MAX_BATCH {
                    match events.try_recv() {
                        Ok(ev) => app.handle(ev),
                        Err(_) => break,
                    }
                }
            }
            _ = ticker.tick(), if app.needs_tick() => app.tick(),
        }
    }
}

/// Creates the app event channel.
pub fn event_channel() -> (mpsc::Sender<AppEvent>, mpsc::Receiver<AppEvent>) {
    mpsc::channel(EVENT_CHANNEL_CAPACITY)
}

/// Forwards crossterm terminal events into `tx` until either side closes.
pub async fn forward_terminal_events(tx: mpsc::Sender<AppEvent>) {
    let mut stream = crossterm::event::EventStream::new();
    while let Some(ev) = stream.next().await {
        match ev {
            Ok(ev) => {
                if tx.send(AppEvent::Terminal(ev)).await.is_err() {
                    break;
                }
            }
            Err(e) => {
                tracing::warn!("terminal input error: {e}");
                break;
            }
        }
    }
}

/// Forwards run events from `RunRecorder::subscribe()` into `tx`. A lagging
/// UI skips events (logged) rather than slowing the core down.
pub async fn forward_run_events(mut rx: broadcast::Receiver<RunEvent>, tx: mpsc::Sender<AppEvent>) {
    loop {
        match rx.recv().await {
            Ok(ev) => {
                if tx.send(AppEvent::Run(Box::new(ev))).await.is_err() {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("UI lagged behind the run; skipped {n} events");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Forwards SIGCONT as [`AppEvent::Resumed`] and SIGTERM / SIGHUP as
/// [`AppEvent::Shutdown`].
pub async fn forward_signals(tx: mpsc::Sender<AppEvent>) -> io::Result<()> {
    use nix::sys::signal::Signal;
    use tokio::signal::unix::{SignalKind, signal};
    let mut cont = signal(SignalKind::from_raw(Signal::SIGCONT as i32))?;
    let mut term = signal(SignalKind::terminate())?;
    let mut hup = signal(SignalKind::hangup())?;
    loop {
        let ev = tokio::select! {
            _ = cont.recv() => AppEvent::Resumed,
            _ = term.recv() => AppEvent::Shutdown,
            _ = hup.recv() => AppEvent::Shutdown,
        };
        if tx.send(ev).await.is_err() {
            return Ok(());
        }
    }
}

/// Runs the TUI on the real terminal. `events`/`tx` are the two ends of
/// [`event_channel`]; give clones of `tx` to [`forward_run_events`] and to
/// components that send [`AppCommand`]s. The terminal is restored on return.
pub async fn run(
    mut app: App,
    mut events: mpsc::Receiver<AppEvent>,
    tx: mpsc::Sender<AppEvent>,
) -> io::Result<()> {
    let mut session = TerminalSession::enter(CrosstermControl::default(), app.mouse_enabled())?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;
    let input = tokio::spawn(forward_terminal_events(tx.clone()));
    let signals = tokio::spawn(forward_signals(tx));
    let result = run_loop(&mut terminal, session.control_mut(), &mut app, &mut events).await;
    input.abort();
    signals.abort();
    let restored = session.finish();
    result.and(restored)
}

/// Shared log of [`TerminalControl`] calls, for tests.
#[derive(Debug, Clone, Default)]
pub struct RecordingControl {
    calls: Arc<std::sync::Mutex<Vec<String>>>,
}

impl RecordingControl {
    /// The calls so far, e.g. `["enter mouse=true", "suspend"]`.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }

    fn log(&self, s: String) {
        if let Ok(mut c) = self.calls.lock() {
            c.push(s);
        }
    }
}

impl TerminalControl for RecordingControl {
    fn enter(&mut self, mouse: bool) -> io::Result<()> {
        self.log(format!("enter mouse={mouse}"));
        Ok(())
    }
    fn leave(&mut self) -> io::Result<()> {
        self.log("leave".into());
        Ok(())
    }
    fn set_mouse_capture(&mut self, on: bool) -> io::Result<()> {
        self.log(format!("mouse={on}"));
        Ok(())
    }
    fn suspend(&mut self) -> io::Result<()> {
        self.log("suspend".into());
        Ok(())
    }
    fn write_osc(&mut self, osc: &str) -> io::Result<()> {
        self.log(format!("osc {}", osc.escape_debug()));
        Ok(())
    }
    fn edit(&mut self, path: &std::path::Path) -> io::Result<()> {
        self.log(format!("edit {}", path.display()));
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod selection_tests;
