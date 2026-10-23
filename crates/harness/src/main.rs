//! `harness` binary: CLI entry point.
//!
//! Subcommands (see `harness --help` and `docs/cli.md`):
//! - `harness` (no arguments): the interactive TUI.
//! - `harness run [OPTIONS] <PROMPT>`: headless agent run (`docs/headless.md`).
//! - `harness wf list|help|run`: workflows (`docs/workflows.md`).
//! - `harness eval run|compare`: eval suites (`docs/evals.md`).
//! - `harness login|logout <provider>`: API key login (`docs/providers.md`).
//! - `harness doctor`, `harness lint`, `harness config check|show`,
//!   `harness schema`, `harness runs show`, `harness sessions`,
//!   `harness ui-demo`, `harness remote-agent --stdio`, `--version`.
//!
//! `__sandbox-exec` (the bash sandbox launcher) is handled before any other
//! initialisation, and `remote-agent` before harness.ini is read. Startup
//! otherwise loads harness.ini, opens the data directory, initialises file
//! logging with credential redaction and installs the terminal-safe panic
//! hook (see `docs/logging.md`).

mod cli;
mod commands;
mod core;
mod dream;
mod headless;
mod ledger;
mod login;
mod memory;
mod remote;
mod remote_agent;
mod sessions;
mod tui;
mod verify;
mod wiring;
mod workflow;

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use harness_config::ini::Config;
use harness_config::paths::DataDir;
use harness_events::logging::{self, LogConfig};

const USAGE: &str = "\
usage: harness [<command> [args]]

With no command, starts the interactive terminal UI in the current directory.

Commands:
  run [OPTIONS] <PROMPT>     run an agent headlessly (harness run --help)
  wf list|help|run           list, describe or run workflows (harness wf --help)
  eval run|compare           run eval suites, compare results (harness eval --help)
  doctor                     check config, credentials, providers, sandbox, remotes, prompts
  lint                       lint prompts, agents, skills and memory
  models [--all] [--json]    list usable models from every provider (--refresh, --offline)
  login <PROVIDER>           check and save an API key for anthropic, openai or openrouter
  logout <PROVIDER>          remove a provider's stored credential
  config check [PATH]        validate harness.ini
  config show [PATH]         print the effective, redacted config with sources
  schema [KIND]              print the JSON schema of prompt, agent or workflow files
  runs show <ID> [--json]    print a run as a tree (or its state as JSON)
  sessions                   list saved sessions by name, most recently active first
  ui-demo [--gallery]        play a scripted demo run in the TUI (no model needed)
  remote-agent --stdio       the remote helper (started over SSH)
  --version                  print the version and git commit";

fn code(c: i32) -> ExitCode {
    ExitCode::from(u8::try_from(c).unwrap_or(1))
}

/// Loads harness.ini, resolves the credentials (so the log redactor knows
/// them), opens the data directory and starts logging.
fn startup() -> Result<(Config, DataDir, Vec<String>), String> {
    let cfg = harness_config::ini::load().map_err(|e| format!("harness.ini:\n{e}"))?;
    let secrets = wiring::credentials(&cfg);
    let data = DataDir::from_config(&cfg)
        .open()
        .map_err(|e| format!("data directory: {e}"))?;
    let log = LogConfig::new(data.logs()).with_redactor(wiring::redactor(&secrets));
    if let Err(err) = logging::init(&log) {
        eprintln!("harness: logging disabled: {err}");
    }
    Ok((cfg, data, secrets))
}

/// [`startup`] or print the error and exit 1.
fn startup_or_exit() -> (Config, DataDir, Vec<String>) {
    startup().unwrap_or_else(|e| {
        eprintln!("harness: {e}");
        std::process::exit(i32::from(cli::exit::ERROR))
    })
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("harness: cannot start the async runtime: {e}");
            std::process::exit(1)
        })
}

fn current_dir() -> std::path::PathBuf {
    wiring::project_dir(None).unwrap_or_else(|e| {
        eprintln!("harness: {e}");
        std::process::exit(i32::from(cli::exit::ERROR))
    })
}

fn cmd_run(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", cli::RUN_USAGE);
        return ExitCode::SUCCESS;
    }
    let run_args = match cli::parse_run(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("harness run: {e}\n\n{}", cli::RUN_USAGE);
            return ExitCode::from(cli::exit::USAGE);
        }
    };
    let (cfg, data, secrets) = startup_or_exit();
    harness_tui::terminal::install_panic_hook();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "harness run starting");
    let redactor = wiring::redactor(&secrets);
    let rc = runtime().block_on(headless::run(run_args, cfg, data, secrets, redactor));
    ExitCode::from(rc)
}

fn cmd_wf(args: &[String]) -> ExitCode {
    let (cfg, data, secrets) = startup_or_exit();
    harness_tui::terminal::install_panic_hook();
    let redactor = wiring::redactor(&secrets);
    let rc = runtime().block_on(workflow::main(
        args,
        cfg,
        data,
        secrets,
        redactor,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    ));
    ExitCode::from(rc)
}

fn cmd_eval(args: &[String]) -> ExitCode {
    let (cfg, data, _) = startup_or_exit();
    let catalog = wiring::catalog(&cfg, &data, &current_dir());
    code(runtime().block_on(commands::eval(
        &cfg,
        &catalog,
        args,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )))
}

fn cmd_tui(args: &[String]) -> ExitCode {
    let opts = match tui::parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("harness: {e}\n\n{USAGE}");
            return ExitCode::from(cli::exit::USAGE);
        }
    };
    let (cfg, data, secrets) = startup_or_exit();
    harness_tui::terminal::install_panic_hook();
    // Personal credentials (Sign in with ChatGPT) are for interactive use.
    wiring::set_interactive(true);
    let redactor = wiring::redactor(&secrets);
    ExitCode::from(runtime().block_on(tui::run(opts, cfg, data, secrets, redactor)))
}

fn main() -> ExitCode {
    // Before any thread exists: the OS refuses the lookup afterwards.
    sessions::init_local_offset();
    let raw: Vec<OsString> = std::env::args_os().collect();
    if raw
        .get(1)
        .is_some_and(|a| a == harness_policy::sandbox::LAUNCHER_ARG)
    {
        return code(harness_policy::sandbox::launcher_main(
            raw.into_iter().skip(2),
        ));
    }
    let args: Vec<String> = raw
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let (stdout, stderr) = (&mut std::io::stdout(), &mut std::io::stderr());
    let sub = args.get(1).map(String::as_str);
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!(
                "harness {} ({})",
                env!("CARGO_PKG_VERSION"),
                env!("HARNESS_GIT_COMMIT")
            );
            ExitCode::SUCCESS
        }
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("remote-agent") => remote_agent::run(&args[1..]),
        Some("run") => cmd_run(&args[1..]),
        Some("wf") => cmd_wf(&args[1..]),
        Some("eval") => cmd_eval(&args[1..]),
        Some("sessions") => match args[1..].first().map(String::as_str) {
            None => {
                let (_, data, _) = startup_or_exit();
                ExitCode::from(headless::sessions(&data, stdout))
            }
            Some("--help" | "-h") => {
                println!("{}", headless::SESSIONS_USAGE);
                ExitCode::SUCCESS
            }
            Some(a) => {
                eprintln!(
                    "harness sessions: unexpected argument {a}\n\n{}",
                    headless::SESSIONS_USAGE
                );
                ExitCode::from(cli::exit::USAGE)
            }
        },
        Some("config") if sub == Some("check") => {
            let path = args.get(2).map(Path::new);
            code(harness_config::ini::run_check(path, stdout, stderr))
        }
        Some("config") if sub == Some("show") => {
            let path = args.get(2).map(Path::new);
            code(harness_config::effective::run_show(path, stdout, stderr))
        }
        Some("schema") => code(harness_config::yaml::run_schema(sub, stdout, stderr)),
        Some("lint") => {
            let (cfg, data, _) = startup_or_exit();
            code(commands::lint(&cfg, &data, &current_dir(), stdout))
        }
        Some("doctor") => code(runtime().block_on(commands::doctor(current_dir(), stdout))),
        Some("login") => code(runtime().block_on(login::login_main(
            &args[1..],
            &login::config_path(),
            &mut login::TerminalPrompt,
            &login::LiveVerifier::default(),
            &|k| std::env::var(k).ok(),
            stdout,
            stderr,
        ))),
        Some("logout") => code(login::logout_main(
            &args[1..],
            &login::config_path(),
            &mut login::TerminalPrompt,
            stdout,
            stderr,
        )),
        Some("models") => {
            let (cfg, _, _) = startup_or_exit();
            code(runtime().block_on(commands::models(&cfg, &args[1..], stdout, stderr)))
        }
        Some("runs") => {
            let (_, data, _) = startup_or_exit();
            code(commands::runs(&data, &args[1..], stdout, stderr))
        }
        Some("ui-demo") => tui::demo::run(&args[1..]),
        None => cmd_tui(&[]),
        Some(a) if a.starts_with("--") => cmd_tui(&args),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(cli::exit::USAGE)
        }
    }
}
