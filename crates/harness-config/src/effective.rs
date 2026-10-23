//! The effective configuration: every `harness.ini` value after defaults,
//! with where it came from, plus a stable hash of the configuration and the
//! loaded YAML that produced a run.
//!
//! [`rows`] flattens a [`Config`] into `section.key = value` rows, each with
//! its source (`file:line`, or `default`). Secrets are already redacted by
//! the config's serializer, so the view is safe to print or share.
//! [`render`] formats it for `harness config show` and `/config`.
//!
//! [`config_hash`] is a sha256 over the canonical JSON of the effective
//! config (without machine-specific paths: the config file location,
//! `[general] data_dir` and `[memory] drive`) and of every active prompt,
//! agent, workflow and skill (kind, name, scope and a hash of its text).
//! The same configuration gives the same hash on any machine; any change to
//! a value or a definition changes it. Runs and eval results are stamped
//! with it.

use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::discovery::{Catalog, Entry, Kind};
use crate::ini::{Config, load_location};
use crate::paths::{ConfigLocation, config_location};

/// Source shown for values nobody set.
pub const DEFAULT_SOURCE: &str = "default";

/// One effective value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Ini section (`general`, `provider.anthropic`, `remote.gpu-box`, ...).
    pub section: String,
    /// Ini key.
    pub key: String,
    /// The value as text (lists joined with `, `; secrets `[redacted]`).
    pub value: String,
    /// `file:line` or [`DEFAULT_SOURCE`].
    pub source: String,
}

fn scalar(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(scalar).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

/// Section name in the ini file for a top-level config field (and map key).
fn ini_section(field: &str, name: Option<&str>) -> String {
    match (field, name) {
        ("providers", Some(n)) => format!("provider.{n}"),
        ("remotes", Some(n)) => format!("remote.{n}"),
        ("mcp", Some(n)) => format!("mcp.{n}"),
        (f, _) => f.to_owned(),
    }
}

/// The ini spelling of a serialized field (`bash_allow` is `bash.allow`).
fn ini_key(section: &str, field: &str) -> String {
    if section == "policy" {
        field.replacen('_', ".", 1)
    } else if section.starts_with("remote.") && field.starts_with("policy_") {
        field.replace('_', ".")
    } else {
        field.to_owned()
    }
}

fn source_of(cfg: &Config, section: &str, key: &str) -> String {
    match cfg.origins.get(&format!("{section}.{key}")) {
        Some(loc) => {
            let file = loc
                .file
                .as_deref()
                .map_or_else(|| "<text>".to_owned(), |p| p.display().to_string());
            format!("{file}:{}", loc.line)
        }
        None => DEFAULT_SOURCE.to_owned(),
    }
}

fn push_rows(cfg: &Config, out: &mut Vec<Row>, section: &str, value: &Value) {
    match value {
        Value::Object(m) => {
            for (field, v) in m {
                let k = &ini_key(section, field);
                // Nested objects (pricing entries) become dotted keys.
                if let Value::Object(inner) = v {
                    for (ik, iv) in inner {
                        let key = format!("{k}.{ik}");
                        let mut source = source_of(cfg, section, &key);
                        if source == DEFAULT_SOURCE {
                            source = source_of(cfg, section, k);
                        }
                        out.push(Row {
                            section: section.to_owned(),
                            source,
                            key,
                            value: scalar(iv),
                        });
                    }
                } else {
                    out.push(Row {
                        section: section.to_owned(),
                        key: k.to_owned(),
                        value: scalar(v),
                        source: source_of(cfg, section, k),
                    });
                }
            }
        }
        other => out.push(Row {
            section: section.to_owned(),
            key: String::new(),
            value: scalar(other),
            source: DEFAULT_SOURCE.to_owned(),
        }),
    }
}

/// Every effective value, by section in file order of the reference
/// (`general` first), keys sorted within a section.
pub fn rows(cfg: &Config) -> Vec<Row> {
    let json = serde_json::to_value(cfg).unwrap_or(Value::Null);
    let mut out = Vec::new();
    let Value::Object(top) = json else {
        return out;
    };
    let order = [
        "general",
        "providers",
        "models",
        "pricing",
        "keys",
        "ui",
        "theme",
        "policy",
        "sandbox",
        "verify",
        "context",
        "budget",
        "memory",
        "remotes",
        "mcp",
    ];
    for field in order {
        let Some(v) = top.get(field) else { continue };
        match field {
            "providers" | "remotes" | "mcp" => {
                if let Value::Object(m) = v {
                    for (name, inner) in m {
                        push_rows(cfg, &mut out, &ini_section(field, Some(name)), inner);
                    }
                }
            }
            _ => push_rows(cfg, &mut out, field, v),
        }
    }
    out
}

/// Formats the effective configuration as ini-like text with a source
/// comment per value, followed by the loaded YAML definitions and the
/// config hash.
pub fn render(cfg: &Config, catalog: Option<&Catalog>) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# effective configuration ({})",
        cfg.source.as_deref().map_or_else(
            || "built-in defaults".to_owned(),
            |p| p.display().to_string()
        )
    );
    let rows = rows(cfg);
    let width = rows
        .iter()
        .map(|r| r.key.len() + 3 + r.value.len())
        .max()
        .unwrap_or(0)
        .min(60);
    let mut section = "";
    for r in &rows {
        if r.section != section {
            let _ = writeln!(s, "\n[{}]", r.section);
            section = &r.section;
        }
        let kv = format!("{} = {}", r.key, r.value);
        let _ = writeln!(s, "{kv:<width$}  ; {}", r.source);
    }
    if let Some(c) = catalog {
        let _ = writeln!(s, "\n# loaded definitions");
        for (kind, name, origin) in definitions(c) {
            let _ = writeln!(s, "; {kind} {name}  ({origin})");
        }
    }
    let _ = writeln!(s, "\n; config hash: {}", config_hash(cfg, catalog));
    s
}

fn definitions(c: &Catalog) -> Vec<(&'static str, String, String)> {
    fn rows<T>(
        kind: Kind,
        entries: Vec<&Entry<T>>,
        name: impl Fn(&T) -> &str,
    ) -> Vec<(&'static str, String, String)> {
        entries
            .into_iter()
            .map(|e| {
                (
                    kind.label(),
                    name(&e.item).to_owned(),
                    e.origin.display.clone(),
                )
            })
            .collect()
    }
    let mut out = rows(Kind::Prompt, c.prompts.active(), |p| p.name.as_str());
    out.extend(rows(Kind::Agent, c.agents.active(), |a| a.name.as_str()));
    out.extend(rows(Kind::Workflow, c.workflows.active(), |w| {
        w.name.as_str()
    }));
    out.extend(rows(Kind::Skill, c.skills.active(), |s| s.name.as_str()));
    out
}

fn text_hash(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn yaml_entries<T>(kind: Kind, entries: Vec<&Entry<T>>, name: impl Fn(&T) -> &str) -> Vec<Value> {
    entries
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "kind": kind.label(),
                "name": name(&e.item),
                "scope": e.origin.scope.label(),
                "sha256": text_hash(&e.text),
            })
        })
        .collect()
}

/// The canonical JSON hashed by [`config_hash`].
pub fn hash_input(cfg: &Config, catalog: Option<&Catalog>) -> Value {
    let mut json = serde_json::to_value(cfg).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut json {
        m.remove("source");
        if let Some(Value::Object(g)) = m.get_mut("general") {
            g.remove("data_dir");
        }
        if let Some(Value::Object(mem)) = m.get_mut("memory") {
            mem.remove("drive");
        }
    }
    let yaml: Vec<Value> = catalog.map_or_else(Vec::new, |c| {
        let mut v = yaml_entries(Kind::Prompt, c.prompts.active(), |p| p.name.as_str());
        v.extend(yaml_entries(Kind::Agent, c.agents.active(), |a| {
            a.name.as_str()
        }));
        v.extend(yaml_entries(Kind::Workflow, c.workflows.active(), |w| {
            w.name.as_str()
        }));
        v.extend(yaml_entries(Kind::Skill, c.skills.active(), |s| {
            s.name.as_str()
        }));
        v
    });
    serde_json::json!({ "config": json, "yaml": yaml })
}

/// Stable sha256 (hex) of the effective config and loaded YAML.
pub fn config_hash(cfg: &Config, catalog: Option<&Catalog>) -> String {
    let bytes = serde_json::to_vec(&hash_input(cfg, catalog)).unwrap_or_default();
    hex::encode(Sha256::digest(&bytes))
}

/// Entry point for `harness config show [path]`: prints the effective,
/// redacted config (without YAML) and returns 0, or prints load errors to
/// `err` and returns 1.
pub fn run_show(path: Option<&Path>, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let location = match path {
        Some(p) => ConfigLocation::Explicit(p.to_path_buf()),
        None => config_location(|k| std::env::var_os(k)),
    };
    let cfg = if !location.is_explicit() && !location.path().exists() {
        Config::defaults()
    } else {
        match load_location(&location) {
            Ok(c) => c,
            Err(e) => {
                let _ = writeln!(err, "{e}");
                return 1;
            }
        }
    };
    let _ = write!(out, "{}", render(&cfg, None));
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ini::load_str;

    const INI: &str = "[general]\nmode = ask\n\n[provider.anthropic]\napi_key = sk-ant-very-secret-value\n\n[policy]\nbash.deny = rm -rf*, sudo*\n";

    fn cfg() -> Config {
        load_str(INI, Some(Path::new("/etc/harness.ini"))).unwrap()
    }

    #[test]
    fn mcp_sections_render_with_ini_keys_and_redacted_secrets() {
        let c = load_str(
            "[mcp.gh]\ncommand = gh-mcp\nenv.TOKEN = tok-inline-secret\nenv.OTHER_cmd = pass show x\n[mcp.web]\nurl = https://h/mcp\nheader.Authorization_env = WEB_TOKEN\n",
            Some(Path::new("/etc/harness.ini")),
        )
        .unwrap();
        let rows = rows(&c);
        let get = |s: &str, k: &str| {
            rows.iter()
                .find(|r| r.section == s && r.key == k)
                .cloned()
                .unwrap_or_else(|| panic!("{s} {k}: {rows:#?}"))
        };
        assert_eq!(get("mcp.gh", "command").value, "gh-mcp");
        assert_eq!(get("mcp.gh", "command").source, "/etc/harness.ini:2");
        let tok = get("mcp.gh", "env.TOKEN");
        assert_eq!(tok.value, "[redacted]");
        assert_eq!(tok.source, "/etc/harness.ini:3");
        assert_eq!(get("mcp.gh", "env.OTHER_cmd").value, "pass show x");
        assert_eq!(get("mcp.web", "url").value, "https://h/mcp");
        assert_eq!(
            get("mcp.web", "header.Authorization_env").value,
            "WEB_TOKEN"
        );
        assert_eq!(get("mcp.web", "timeout").source, DEFAULT_SOURCE);
        let text = render(&c, None);
        assert!(text.contains("[mcp.gh]"), "{text}");
        assert!(!text.contains("tok-inline-secret"));
    }

    #[test]
    fn rows_have_values_and_sources_and_secrets_are_redacted() {
        let rows = rows(&cfg());
        let get = |s: &str, k: &str| {
            rows.iter()
                .find(|r| r.section == s && r.key == k)
                .cloned()
                .unwrap()
        };
        let mode = get("general", "mode");
        assert_eq!(mode.value, "ask");
        assert_eq!(mode.source, "/etc/harness.ini:2");
        let model = get("general", "default_model");
        assert_eq!(model.source, DEFAULT_SOURCE);
        let key = get("provider.anthropic", "api_key");
        assert_eq!(key.source, "/etc/harness.ini:5");
        assert!(!key.value.contains("very-secret"));
        let deny = get("policy", "bash.deny");
        assert_eq!(deny.value, "rm -rf*, sudo*");
        assert_eq!(deny.source, "/etc/harness.ini:8");
        let text = render(&cfg(), None);
        assert!(!text.contains("very-secret"));
        assert!(text.contains("[provider.anthropic]"));
        assert!(text.contains("; /etc/harness.ini:2"));
        assert!(text.contains("config hash: "));
    }

    #[test]
    fn hash_is_stable_and_sensitive() {
        let a = config_hash(&cfg(), None);
        assert_eq!(a, config_hash(&cfg(), None));
        assert_eq!(a.len(), 64);
        // The file location does not matter, values do.
        let elsewhere = load_str(INI, Some(Path::new("/other/harness.ini"))).unwrap();
        assert_eq!(config_hash(&elsewhere, None), a);
        let changed = load_str(&INI.replace("ask", "auto"), None).unwrap();
        assert_ne!(config_hash(&changed, None), a);
        // Loaded YAML is part of the hash.
        let catalog = Catalog::load(&crate::builtins::roots());
        let with_yaml = config_hash(&cfg(), Some(&catalog));
        assert_ne!(with_yaml, a);
        assert_eq!(with_yaml, config_hash(&cfg(), Some(&catalog)));
        let mut c2 = cfg();
        c2.general.data_dir = "/elsewhere".into();
        assert_eq!(
            config_hash(&c2, Some(&catalog)),
            with_yaml,
            "data_dir is machine-specific"
        );
    }

    #[test]
    fn hash_changes_when_a_definition_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        std::fs::create_dir_all(&dir).unwrap();
        let roots = crate::builtins::roots().with_global(tmp.path());
        std::fs::write(
            dir.join("x.yaml"),
            "name: x\nprompt: coder\ntools: [read]\n",
        )
        .unwrap();
        let h1 = config_hash(&cfg(), Some(&Catalog::load(&roots)));
        std::fs::write(
            dir.join("x.yaml"),
            "name: x\nprompt: coder\ntools: [read, bash]\n",
        )
        .unwrap();
        let h2 = config_hash(&cfg(), Some(&Catalog::load(&roots)));
        assert_ne!(h1, h2);
    }

    #[test]
    fn run_show_prints_or_reports_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("harness.ini");
        std::fs::write(&p, "[general]\nmode = auto\n").unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run_show(Some(&p), &mut out, &mut err), 0);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("mode = auto"));
        std::fs::write(&p, "[general]\nbogus = 1\n").unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run_show(Some(&p), &mut out, &mut err), 1);
        assert!(String::from_utf8(err).unwrap().contains("bogus"));
    }
}
