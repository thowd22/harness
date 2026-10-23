//! Small subcommands that wrap library entry points: `lint`, `doctor`,
//! `config show`, `runs show`, `models` and `eval`.
//!
//! Each returns its exit code; output goes to the given writers so the
//! commands stay testable.

use std::io::Write;
use std::path::{Path, PathBuf};

use harness_config::discovery::Catalog as DefinitionCatalog;
use harness_config::ini::Config;
use harness_config::lint::{self, LintOptions, LintSource};
use harness_config::paths::DataDir;
use harness_providers::catalog::{Catalog, CatalogEntry};

use crate::wiring;

/// Usage of `harness models`.
pub const MODELS_USAGE: &str = "usage: harness models [--all] [--json] [--refresh | --offline]";

/// `harness models`: the model catalog. Stale live lists are refreshed
/// first (`--refresh` refetches all, `--offline` uses only the cache).
/// Shows usable models unless `--all`.
pub async fn models(
    cfg: &Config,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let has = |f: &str| args.iter().any(|a| a == f);
    let known = ["--all", "--json", "--refresh", "--offline"];
    if args.iter().any(|a| !known.contains(&a.as_str())) || (has("--refresh") && has("--offline")) {
        let _ = writeln!(err, "{MODELS_USAGE}");
        return 2;
    }
    let registry = match wiring::registry(cfg) {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(err, "harness: {e}");
            return 1;
        }
    };
    let service = wiring::model_catalog(cfg, &registry);
    if !has("--offline") {
        let report = service.refresh(has("--refresh")).await;
        for (provider, e) in &report.failed {
            let _ = writeln!(err, "harness: could not refresh {provider} models: {e}");
        }
    }
    let catalog = service.catalog();
    if has("--json") {
        let entries: Vec<&CatalogEntry> = catalog
            .entries()
            .iter()
            .filter(|e| has("--all") || e.usable())
            .collect();
        let _ = serde_json::to_writer_pretty(&mut *out, &entries);
        let _ = writeln!(out);
        return 0;
    }
    render_models(&catalog, has("--all"), out);
    0
}

fn tokens(n: u32) -> String {
    match n {
        0 => "?".into(),
        n if n >= 1_000_000 && n % 1_000_000 == 0 => format!("{}M", n / 1_000_000),
        n if n >= 1_000 => format!("{}k", n / 1_000),
        n => n.to_string(),
    }
}

fn usd(p: Option<f64>, plan: bool) -> String {
    match p {
        _ if plan => "plan".into(),
        Some(v) => format!("${v}"),
        None => "?".into(),
    }
}

/// Prints the catalog as an aligned table grouped by provider.
pub fn render_models(catalog: &Catalog, all: bool, out: &mut dyn Write) {
    use harness_providers::catalog::PriceOrigin;
    let header = [
        "MODEL", "PROVIDER", "ALIAS", "CONTEXT", "IN", "OUT", "CAPS", "AUTH", "STATUS",
    ];
    let mut rows: Vec<[String; 9]> = Vec::new();
    for e in catalog.entries().iter().filter(|e| all || e.usable()) {
        let plan = e.price_origin == PriceOrigin::Plan;
        let c = e.capabilities;
        let caps: Vec<&str> = [
            (c.tools, "tools"),
            (c.reasoning, "reasoning"),
            (c.vision, "vision"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        rows.push([
            e.model().to_owned(),
            e.provider().to_owned(),
            e.aliases.join(","),
            tokens(e.context_window),
            usd(e.input_price(), plan),
            usd(e.output_price(), plan),
            caps.join(","),
            e.auth.to_string(),
            e.reason().unwrap_or_else(|| "ok".into()),
        ]);
    }
    let mut widths = header.map(str::len);
    for r in &rows {
        for (w, cell) in widths.iter_mut().zip(r) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let mut s = String::new();
        for (i, (cell, w)) in cells.iter().zip(widths).enumerate() {
            if i + 1 == cells.len() {
                s.push_str(cell);
            } else {
                s.push_str(&format!("{cell:<w$}  "));
            }
        }
        s
    };
    let _ = writeln!(out, "{}", line(header.to_vec()));
    for r in &rows {
        let _ = writeln!(out, "{}", line(r.iter().map(String::as_str).collect()));
    }
    if rows.is_empty() {
        let _ = writeln!(
            out,
            "no usable models: add an API key to a [provider.*] section of harness.ini (harness models --all lists every model)"
        );
    }
    if catalog.providers().iter().any(|p| p.stale) {
        let stale: Vec<&str> = catalog
            .providers()
            .iter()
            .filter(|p| p.stale)
            .map(|p| p.provider.as_str())
            .collect();
        let _ = writeln!(
            out,
            "live model lists not current for: {}",
            stale.join(", ")
        );
    }
}

/// Usage of `harness runs`.
pub const RUNS_USAGE: &str = "usage: harness runs show <ID|PREFIX|last> [--json]";

/// The memory index (`MEMORY.md`) as a lint source, when memory is enabled
/// and the drive exists. The drive is never created here.
pub fn memory_sources(cfg: &Config) -> Vec<LintSource> {
    if !cfg.memory.enabled {
        return Vec::new();
    }
    let file = DataDir::from_config(cfg)
        .memory()
        .join(harness_memory::drive::MEMORY_FILE);
    match std::fs::read_to_string(&file) {
        Ok(text) => vec![LintSource::memory(
            "memory index",
            file.display().to_string(),
            text,
        )],
        Err(_) => Vec::new(),
    }
}

/// `harness lint`: prompt lint over the built-in, global and trusted
/// project definitions plus the memory index. Exit 1 on errors.
pub fn lint(cfg: &Config, data: &DataDir, cwd: &Path, out: &mut dyn Write) -> i32 {
    let catalog = wiring::catalog(cfg, data, cwd);
    let report = lint::lint_catalog(
        &catalog,
        Some(&wiring::validation_context(cfg)),
        &memory_sources(cfg),
        &LintOptions::default(),
    );
    lint::run_lint(&report, out)
}

/// `harness doctor`: every health check with fixes.
pub async fn doctor(project: PathBuf, out: &mut dyn Write) -> i32 {
    let input = harness_eval::doctor::Input {
        config: None,
        project: Some(project),
        global_dir: None,
    };
    harness_eval::doctor::run(&input, &harness_eval::doctor::LiveProbes::default(), out).await
}

/// `harness runs show <id> [--json]`.
pub fn runs(data: &DataDir, args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let (sub, rest) = match args.split_first() {
        Some((s, r)) => (s.as_str(), r),
        None => ("", args),
    };
    let json = rest.iter().any(|a| a == "--json");
    let ids: Vec<&String> = rest.iter().filter(|a| *a != "--json").collect();
    match (sub, ids.as_slice()) {
        ("show", [id]) if !id.starts_with('-') => {
            harness_tui::history::show::run_show(data.root(), id, json, out, err)
        }
        _ => {
            let _ = writeln!(err, "{RUNS_USAGE}");
            2
        }
    }
}

/// `harness eval ...`, stamped with the effective config hash.
pub async fn eval(
    cfg: &Config,
    catalog: &DefinitionCatalog,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let ctx = harness_eval::cli::Context {
        harness_bin: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("harness")),
        harness_version: env!("CARGO_PKG_VERSION").to_owned(),
        config_hash: harness_config::effective::config_hash(cfg, Some(catalog)),
    };
    harness_eval::cli::main(args, &ctx, out, err).await
}

#[cfg(test)]
mod models_tests {
    use super::*;
    use harness_providers::catalog::{AuthMethod, CredentialState, Credentials};
    use harness_providers::registry::Registry;

    #[test]
    fn models_table_lists_usable_models_with_every_column() {
        let creds =
            Credentials::new().with("anthropic", AuthMethod::ApiKey, CredentialState::Ready);
        let catalog = Catalog::build(&Registry::builtin(), &creds, &Default::default());
        let mut out = Vec::new();
        render_models(&catalog, false, &mut out);
        let text = String::from_utf8(out).unwrap();
        let mut lines = text.lines();
        let header = lines.next().unwrap();
        assert!(header.starts_with("MODEL"), "{text}");
        assert!(header.contains("AUTH") && header.contains("STATUS"));
        let opus = text
            .lines()
            .find(|l| l.starts_with("claude-opus-5-5 "))
            .unwrap();
        for cell in [
            "anthropic",
            "opus",
            "1M",
            "$4",
            "$20",
            "tools,reasoning,vision",
            "API key",
            "ok",
        ] {
            assert!(opus.contains(cell), "{cell} missing: {opus}");
        }
        assert!(!text.contains("gpt-5"), "openai has no key: {text}");

        let mut out = Vec::new();
        render_models(&catalog, true, &mut out);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("no openai API key"), "{text}");
    }

    #[tokio::test]
    async fn models_command_rejects_unknown_flags() {
        let cfg = harness_config::ini::load_str("", None).unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            models(&cfg, &["--bogus".into()], &mut out, &mut err).await,
            2
        );
        assert!(
            String::from_utf8(err)
                .unwrap()
                .contains("usage: harness models")
        );
    }
}
