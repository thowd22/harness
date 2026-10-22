//! System prompt YAML (`prompts/*.yaml`): ordered layers and an allowlisted
//! set of template variables.
//!
//! ```yaml
//! name: coder
//! description: Default coding prompt
//! layers:                 # concatenated in order
//!   - file: base.md       # relative to this yaml
//!   - text: |
//!       You are working in {{ cwd }} on {{ os }}. Today is {{ date }}.
//! vars: [cwd, os, date]   # must be a subset of ALLOWED_VARS
//! ```
//!
//! Templates use minijinja with no filters, functions or globals, and with
//! strict undefined handling. The only values in scope are the variables a
//! prompt declares in `vars`, which must come from [`ALLOWED_VARS`].
//! Environment variables and secrets are never reachable.
//!
//! A session renders its prompt once into a [`SessionPrompt`] from a frozen
//! [`PromptVars`] snapshot, so the text is byte-identical on every turn
//! (provider prompt caching depends on it), even if the YAML is hot-reloaded.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::discovery::{Artifact, Kind, SourceFile};
use crate::yaml::YamlError;

/// Template variables a prompt may declare. Nothing else is ever exposed.
pub const ALLOWED_VARS: &[&str] = &["cwd", "os", "date", "git_branch"];

/// Separator placed between rendered layers.
pub const LAYER_SEPARATOR: &str = "\n\n";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptDoc {
    name: String,
    #[serde(default)]
    description: Option<String>,
    layers: Vec<LayerDoc>,
    #[serde(default)]
    vars: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerDoc {
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

/// Where a layer's text came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerSource {
    /// `text:` in the YAML.
    Inline,
    /// `file:` relative to the YAML (as written).
    File(String),
}

/// One prompt layer: an unrendered template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Layer {
    /// Origin of the text.
    pub source: LayerSource,
    /// Template text, as read.
    pub template: String,
}

/// A loaded, validated system prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Prompt {
    /// Name used by agents' `prompt:`.
    pub name: String,
    /// Description.
    pub description: Option<String>,
    /// Layers in order.
    pub layers: Vec<Layer>,
    /// Declared template variables (subset of [`ALLOWED_VARS`]).
    pub vars: Vec<String>,
}

/// Values for the allowlisted template variables, captured once per session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptVars {
    /// Working directory.
    pub cwd: String,
    /// Operating system (`linux`, `macos`, ...).
    pub os: String,
    /// Session start date, `YYYY-MM-DD` (UTC).
    pub date: String,
    /// Current git branch; empty when unknown, `HEAD` when detached.
    pub git_branch: String,
}

impl PromptVars {
    /// Captures the variables for `cwd` now. No subprocess is run; the git
    /// branch is read from `.git/HEAD`.
    pub fn detect(cwd: &Path) -> Self {
        let date = time::OffsetDateTime::now_utc().date();
        PromptVars {
            cwd: cwd.display().to_string(),
            os: std::env::consts::OS.to_owned(),
            date: format!(
                "{:04}-{:02}-{:02}",
                date.year(),
                u8::from(date.month()),
                date.day()
            ),
            git_branch: git_branch(cwd).unwrap_or_default(),
        }
    }

    fn get(&self, name: &str) -> Option<&str> {
        match name {
            "cwd" => Some(&self.cwd),
            "os" => Some(&self.os),
            "date" => Some(&self.date),
            "git_branch" => Some(&self.git_branch),
            _ => None,
        }
    }
}

/// The current branch of the repository containing `dir`, from `.git/HEAD`
/// (worktrees with a `.git` file are followed). `HEAD` when detached.
pub fn git_branch(dir: &Path) -> Option<String> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let dotgit = d.join(".git");
        let git_dir = if dotgit.is_dir() {
            Some(dotgit)
        } else if dotgit.is_file() {
            let text = std::fs::read_to_string(&dotgit).ok()?;
            let p = text.trim().strip_prefix("gitdir:")?.trim().to_owned();
            Some(d.join(p))
        } else {
            None
        };
        if let Some(g) = git_dir {
            let head = std::fs::read_to_string(g.join("HEAD")).ok()?;
            let head = head.trim();
            return Some(match head.strip_prefix("ref: refs/heads/") {
                Some(b) => b.to_owned(),
                None => "HEAD".to_owned(),
            });
        }
        cur = d.parent();
    }
    None
}

/// An error rendering a prompt (only possible for prompts built by hand;
/// loaded prompts are checked at load time).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("prompt `{prompt}` layer {layer}: {message}")]
pub struct RenderError {
    /// Prompt name.
    pub prompt: String,
    /// 1-based layer index.
    pub layer: usize,
    /// Message.
    pub message: String,
}

fn env<'a>() -> minijinja::Environment<'a> {
    let mut env = minijinja::Environment::empty();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    env
}

impl Prompt {
    /// Renders all layers and joins them with [`LAYER_SEPARATOR`]. Trailing
    /// whitespace of each layer is trimmed so file endings do not matter.
    /// Deterministic: equal inputs give byte-identical output.
    pub fn render(&self, vars: &PromptVars) -> Result<String, RenderError> {
        let ctx: BTreeMap<String, String> = self
            .vars
            .iter()
            .filter_map(|v| Some((v.clone(), vars.get(v)?.to_owned())))
            .collect();
        let ctx = minijinja::Value::from(ctx);
        let env = env();
        let mut out = String::new();
        for (i, layer) in self.layers.iter().enumerate() {
            let err = |e: minijinja::Error| RenderError {
                prompt: self.name.clone(),
                layer: i + 1,
                message: e.to_string(),
            };
            let text = env.render_str(&layer.template, ctx.clone()).map_err(err)?;
            let text = text.trim_end();
            if text.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push_str(LAYER_SEPARATOR);
            }
            out.push_str(text);
        }
        Ok(out)
    }

    /// Concatenated raw template text of all layers (for lint).
    pub fn raw_text(&self) -> String {
        self.layers
            .iter()
            .map(|l| l.template.as_str())
            .collect::<Vec<_>>()
            .join(LAYER_SEPARATOR)
    }

    /// The JSON Schema for prompt files.
    pub fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": "https://harness.dev/schema/prompt.json",
            "title": "harness prompt",
            "description": "System prompt: ordered layers concatenated with a blank line between them.",
            "type": "object",
            "additionalProperties": false,
            "required": ["name", "layers"],
            "properties": {
                "name": { "type": "string", "minLength": 1, "description": "Name agents refer to with `prompt:`." },
                "description": { "type": "string" },
                "layers": {
                    "type": "array",
                    "minItems": 1,
                    "description": "Concatenated in order; the order is stable for prompt caching.",
                    "items": {
                        "oneOf": [
                            {
                                "type": "object", "additionalProperties": false, "required": ["file"],
                                "properties": { "file": { "type": "string", "description": "Markdown file relative to this YAML." } }
                            },
                            {
                                "type": "object", "additionalProperties": false, "required": ["text"],
                                "properties": { "text": { "type": "string", "description": "Inline template text." } }
                            }
                        ]
                    }
                },
                "vars": {
                    "type": "array",
                    "uniqueItems": true,
                    "description": "Template variables this prompt uses. No environment variables or secrets.",
                    "items": { "enum": ALLOWED_VARS }
                }
            }
        })
    }
}

impl Artifact for Prompt {
    const KIND: Kind = Kind::Prompt;

    fn load(src: &SourceFile) -> Result<Self, YamlError> {
        let doc: PromptDoc = src.parse_yaml(&src.text)?;
        if doc.name.trim().is_empty() {
            return Err(src.error_at(&["name"], "`name` must not be empty"));
        }
        if doc.layers.is_empty() {
            return Err(src.error_at(
                &["layers"],
                "`layers` must list at least one `file:` or `text:` layer",
            ));
        }
        for v in &doc.vars {
            if !ALLOWED_VARS.contains(&v.as_str()) {
                return Err(src.error_at(
                    &["vars"],
                    format!(
                        "`{v}` is not an allowed template variable; allowed: {}. \
                         Environment variables and secrets are never available to prompts",
                        ALLOWED_VARS.join(", ")
                    ),
                ));
            }
        }
        let env = env();
        let mut layers = Vec::with_capacity(doc.layers.len());
        for (i, l) in doc.layers.into_iter().enumerate() {
            let (source, template) = match (l.file, l.text) {
                (None, Some(t)) => (LayerSource::Inline, t),
                (Some(_), Some(_)) | (None, None) => {
                    return Err(src.error_at(
                        &["layers"],
                        format!("layer {}: set exactly one of `file:` or `text:`", i + 1),
                    ));
                }
                (Some(f), None) => {
                    let text = src
                        .read_relative(&f)
                        .map_err(|m| src.error_at(&["layers"], format!("layer {}: {m}", i + 1)))?;
                    (LayerSource::File(f), text)
                }
            };
            let what = match &source {
                LayerSource::Inline => format!("layer {}", i + 1),
                LayerSource::File(f) => format!("layer {} (`{f}`)", i + 1),
            };
            let tmpl = env.template_from_str(&template).map_err(|e| {
                let at = e.line().map(|l| format!(" line {l}")).unwrap_or_default();
                src.error_at(
                    &["layers"],
                    format!(
                        "{what}: template error at{at}: {}",
                        e.detail().unwrap_or("syntax error")
                    ),
                )
            })?;
            let mut used: Vec<String> = tmpl.undeclared_variables(false).into_iter().collect();
            used.sort();
            for u in used {
                if !doc.vars.contains(&u) {
                    let hint = if ALLOWED_VARS.contains(&u.as_str()) {
                        format!("add it to `vars: [{u}]`")
                    } else {
                        format!("allowed variables are {}", ALLOWED_VARS.join(", "))
                    };
                    return Err(src.error_at(
                        &["layers"],
                        format!("{what} uses undeclared variable `{u}`; {hint}"),
                    ));
                }
            }
            layers.push(Layer { source, template });
        }
        Ok(Prompt {
            name: doc.name,
            description: doc.description,
            layers,
            vars: doc.vars,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// A prompt rendered once for a session. The text never changes for the
/// session's lifetime, so every turn sends a byte-identical system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPrompt {
    name: String,
    vars: PromptVars,
    text: String,
    sha256: String,
}

impl SessionPrompt {
    /// Renders `prompt` with the frozen `vars`.
    pub fn new(prompt: &Prompt, vars: PromptVars) -> Result<Self, RenderError> {
        let text = prompt.render(&vars)?;
        let sha256 = hex::encode(Sha256::digest(text.as_bytes()));
        Ok(SessionPrompt {
            name: prompt.name.clone(),
            vars,
            text,
            sha256,
        })
    }

    /// The rendered system prompt.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Prompt name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The frozen variables.
    pub fn vars(&self) -> &PromptVars {
        &self.vars
    }

    /// sha256 (hex) of the text, for cache-stability checks.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{BuiltinFile, Scope};

    fn vars() -> PromptVars {
        PromptVars {
            cwd: "/work".into(),
            os: "linux".into(),
            date: "2026-10-22".into(),
            git_branch: "main".into(),
        }
    }

    fn load(text: &str) -> Result<Prompt, YamlError> {
        Prompt::load(&SourceFile::inline(
            Scope::Global,
            Kind::Prompt,
            "prompts/p.yaml",
            text,
        ))
    }

    static FILES: &[BuiltinFile] = &[
        BuiltinFile {
            path: "prompts/base.md",
            contents: "# Base\nBe concise.\n\n",
        },
        BuiltinFile {
            path: "prompts/coder.yaml",
            contents: "name: coder\nlayers:\n  - file: base.md\n  - text: |\n      In {{ cwd }} on {{ os }} ({{ git_branch }}), {{ date }}.\nvars: [cwd, os, date, git_branch]\n",
        },
    ];

    #[test]
    fn ordered_layers_from_file_and_text() {
        let src = SourceFile::builtin(Kind::Prompt, &FILES[1], FILES);
        let p = Prompt::load(&src).unwrap();
        assert_eq!(p.layers.len(), 2);
        assert_eq!(p.layers[0].source, LayerSource::File("base.md".into()));
        assert_eq!(p.layers[1].source, LayerSource::Inline);
        assert_eq!(
            p.render(&vars()).unwrap(),
            "# Base\nBe concise.\n\nIn /work on linux (main), 2026-10-22."
        );
    }

    #[test]
    fn file_layers_from_disk_relative_to_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let pdir = dir.path().join("prompts");
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(pdir.join("a.md"), "A").unwrap();
        std::fs::write(
            pdir.join("p.yaml"),
            "name: p\nlayers: [{file: a.md}, {text: B}]\n",
        )
        .unwrap();
        let src = SourceFile::from_path(
            Scope::Global,
            Kind::Prompt,
            dir.path(),
            &pdir.join("p.yaml"),
        )
        .unwrap();
        assert_eq!(
            Prompt::load(&src).unwrap().render(&vars()).unwrap(),
            "A\n\nB"
        );
    }

    #[test]
    fn file_layer_cannot_escape_or_be_missing() {
        let src = SourceFile::builtin(Kind::Prompt, &FILES[1], FILES);
        for bad in ["../x.md", "/etc/passwd"] {
            let s = src
                .clone()
                .with_text(format!("name: x\nlayers:\n  - file: {bad}\n"));
            let e = Prompt::load(&s).unwrap_err();
            assert!(e.message.contains("relative path"), "{e}");
            assert_eq!(e.line, 2, "{e}");
        }
        let s = src.with_text("name: x\nlayers:\n  - file: nope.md\n");
        assert!(Prompt::load(&s).unwrap_err().message.contains("not found"));
    }

    #[test]
    fn project_symlinked_layer_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let pdir = dir.path().join("prompts");
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(dir.path().join("secret.md"), "S").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret.md"), pdir.join("a.md")).unwrap();
        std::fs::write(pdir.join("p.yaml"), "name: p\nlayers: [{file: a.md}]\n").unwrap();
        let src = SourceFile::from_path(
            Scope::Project,
            Kind::Prompt,
            dir.path(),
            &pdir.join("p.yaml"),
        )
        .unwrap();
        assert!(Prompt::load(&src).unwrap_err().message.contains("symlink"));
    }

    #[test]
    fn vars_outside_allowlist_are_rejected() {
        let e = load("name: x\nlayers: [{text: hi}]\nvars: [cwd, env]\n").unwrap_err();
        assert_eq!(e.line, 3);
        assert!(
            e.message
                .contains("`env` is not an allowed template variable"),
            "{e}"
        );
    }

    #[test]
    fn undeclared_or_secret_lookups_are_rejected() {
        let e = load("name: x\nlayers:\n  - text: \"{{ cwd }}\"\n").unwrap_err();
        assert!(
            e.message
                .contains("undeclared variable `cwd`; add it to `vars: [cwd]`"),
            "{e}"
        );
        let e = load("name: x\nlayers:\n  - text: \"{{ env.HOME }}\"\nvars: [cwd]\n").unwrap_err();
        assert!(e.message.contains("undeclared variable `env`"), "{e}");
        // No functions or filters exist, so nothing can reach the environment.
        let e = load("name: x\nlayers:\n  - text: \"{{ cwd | upper }}\"\nvars: [cwd]\n");
        let rendered = e.ok().map(|p| p.render(&vars()));
        assert!(matches!(rendered, Some(Err(_))), "filters must not exist");
    }

    #[test]
    fn template_syntax_error_is_located() {
        let e = load("name: x\nlayers:\n  - text: \"{{ cwd \"\nvars: [cwd]\n").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(e.message.contains("template error"), "{e}");
    }

    #[test]
    fn unknown_keys_and_empty_layers_are_errors() {
        assert!(
            load("name: x\nlayer: []\n")
                .unwrap_err()
                .message
                .contains("unknown field `layer`")
        );
        assert!(
            load("name: x\nlayers: []\n")
                .unwrap_err()
                .message
                .contains("at least one")
        );
        assert!(load("name: x\nlayers: [{txt: a}]\n").is_err());
    }

    #[test]
    fn session_prompt_is_byte_identical_across_turns() {
        let src = SourceFile::builtin(Kind::Prompt, &FILES[1], FILES);
        let p = Prompt::load(&src).unwrap();
        let session = SessionPrompt::new(&p, vars()).unwrap();
        let turns: Vec<String> = (0..5).map(|_| session.text().to_owned()).collect();
        assert!(
            turns
                .iter()
                .all(|t| t.as_bytes() == session.text().as_bytes())
        );
        // Re-rendering from the same inputs is deterministic, too.
        let again = SessionPrompt::new(&p, session.vars().clone()).unwrap();
        assert_eq!(again.text().as_bytes(), session.text().as_bytes());
        assert_eq!(again.sha256(), session.sha256());
        assert_eq!(session.sha256().len(), 64);
    }

    #[test]
    fn detect_and_git_branch() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a/b");
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        assert_eq!(git_branch(&sub).as_deref(), Some("feature/x"));
        std::fs::write(dir.path().join(".git/HEAD"), "0123456789abcdef\n").unwrap();
        assert_eq!(git_branch(&sub).as_deref(), Some("HEAD"));
        let v = PromptVars::detect(&sub);
        assert_eq!(v.os, std::env::consts::OS);
        assert_eq!(v.date.len(), 10);
        assert_eq!(v.cwd, sub.display().to_string());
    }
}
