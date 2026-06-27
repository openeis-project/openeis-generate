//! Per-template configuration, ported from cargo-generate's `config.rs` but
//! deserialized from [KDL](https://kdl.dev) instead of TOML.
//!
//! ## Why a private `ConfigDeser` + public `Config` split
//!
//! `kdl` 6.7.1's serde deserializer has a bug: `#[serde(default)]` on `Vec`/`bool`
//! fields of a *nested* struct makes it feed the bool's default (`false`) into a
//! `Vec` slot (`invalid type: boolean false, expected a sequence`). To avoid it
//! completely, the deserialization struct uses `Option<…>` for every field and
//! **no `#[serde(default)]` at all**. The public [`Config`] then normalizes
//! `template` to a non-`Option` for an ergonomic API.
//!
//! Optimizations retained vs. the cargo-generate original:
//!  2. `placeholders` is a strongly-typed `IndexMap<String, Placeholder>`
//!     instead of `IndexMap<String, toml::Value>` (compile-time validation).
//!  3. The three copy-pasted hook getters collapse into one phase-keyed getter.
//! Optimization 1 (`#[serde(default)] Vec`) was reverted — it triggers the bug
//! above. Lists therefore use the multi-argument node form
//! (`include "a" "b"`), not repeated node names.
//!
//! ## KDL authoring gotchas (kdl 6.7.1 parser)
//!
//! * **Booleans are `#true`/`#false`, not bare `true`/`false`.** kdl 6.7.1's v2
//!   parser treats bare `true`/`false`/`null` as identifiers rather than values,
//!   so write boolean values with the `#` prefix: `default #false`, `init #true`
//!   (works both inline and on its own line). We deliberately do NOT enable the
//!   `v1-fallback` feature: it would accept bare bools but disables v2-only
//!   syntax — notably hash-strings (see the next bullet).
//! * **Lists are multi-argument nodes:** `include "a" "b"`. Repeated node names
//!   (`include "a"` / `include "b"`) don't deserialize into `Option<Vec<_>>`.
//! * **Conditional expressions: use a KDL hash-string to avoid escaping.** A
//!   `conditional` key is a KDL string holding a rhai expression, which usually
//!   contains its own quoted string literals (`database != "sqlite"`). Writing
//!   that as an ordinary KDL string forces backslash escapes:
//!   `"database != \"sqlite\""`. Instead use a hash-string — `#"…"#` — where
//!   inner double-quotes need no escaping:
//!   `#"database != "sqlite""#`. kdl 6.7.1 parses it to the identical value, so
//!   prefer it for any conditional key that contains quotes.

use std::convert::TryFrom;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::fs;

use anyhow::{bail, Result};
use indexmap::IndexMap;
use semver::VersionReq;
use serde::Deserialize;

use crate::vcs::Vcs;

/// Name of the per-template configuration file.
pub const CONFIG_FILE_NAME: &str = "template.kdl";

/// Public config. `template` is always present (defaults to empty).
#[derive(Debug, PartialEq, Default, Clone)]
pub struct Config {
    pub template: TemplateConfig,
    pub placeholders: Option<IndexMap<String, Placeholder>>,
    pub hooks: Option<HooksConfig>,
    pub conditional: Option<IndexMap<String, ConditionalConfig>>,
}

/// Private deserialization shape: every field is `Option`, **no `#[serde(default)]`**,
/// to dodge the kdl-serde default-bleeding bug (see module docs).
#[derive(Deserialize, Default)]
struct ConfigDeser {
    template: Option<TemplateConfig>,
    placeholders: Option<IndexMap<String, Placeholder>>,
    hooks: Option<HooksConfig>,
    conditional: Option<IndexMap<String, ConditionalConfig>>,
}

#[derive(Deserialize, Debug, PartialEq, Eq, Default, Clone)]
pub struct TemplateConfig {
    pub sub_templates: Option<Vec<String>>,
    pub generator_version: Option<VersionReq>,
    pub include: Option<Vec<String>>,
    pub exclude: Option<Vec<String>>,
    pub ignore: Option<Vec<String>>,
    pub vcs: Option<Vcs>,
    /// `init #false` → `Some(false)`; `init #true` / `init` → `Some(true)`.
    pub init: Option<bool>,
}

#[derive(Deserialize, Debug, PartialEq, Default, Clone)]
pub struct ConditionalConfig {
    pub include: Option<Vec<String>>,
    pub exclude: Option<Vec<String>>,
    pub ignore: Option<Vec<String>>,
    pub placeholders: Option<IndexMap<String, Placeholder>>,
}

/// Strongly-typed placeholder definition (optimization 2).
#[derive(Deserialize, Debug, PartialEq, Clone)]
pub struct Placeholder {
    /// `"string"` or `"bool"`.
    #[serde(rename = "type")]
    pub r#type: String,
    pub prompt: String,
    pub default: Option<serde_json::Value>,
    pub choices: Option<Vec<serde_json::Value>>,
    pub regex: Option<String>,
}

impl Placeholder {
    pub fn is_bool(&self) -> bool {
        self.r#type.eq_ignore_ascii_case("bool")
    }

    pub fn is_string(&self) -> bool {
        self.r#type.eq_ignore_ascii_case("string")
    }

    /// `"array"` — a multi-select placeholder. Its value is stored as a
    /// comma-joined string (see [`crate::variables::resolve_array`]).
    pub fn is_array(&self) -> bool {
        self.r#type.eq_ignore_ascii_case("array")
    }
}

/// Hook execution phase. Optimization 3: one keyed getter replaces three
/// near-identical `get_{init,pre,post}_hooks` methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    Init,
    Pre,
    Post,
}

#[derive(Deserialize, Debug, PartialEq, Eq, Default, Clone)]
pub struct HooksConfig {
    init: Option<Vec<String>>,
    pre: Option<Vec<String>>,
    post: Option<Vec<String>>,
}

impl HooksConfig {
    pub fn get(&self, phase: HookPhase) -> &[String] {
        match phase {
            HookPhase::Init => self.init.as_deref().unwrap_or(&[]),
            HookPhase::Pre => self.pre.as_deref().unwrap_or(&[]),
            HookPhase::Post => self.post.as_deref().unwrap_or(&[]),
        }
    }

    /// All hook commands across every phase, in execution order (init → pre → post).
    pub fn all(&self) -> Vec<&str> {
        [HookPhase::Init, HookPhase::Pre, HookPhase::Post]
            .into_iter()
            .flat_map(|phase| self.get(phase).iter().map(String::as_str))
            .collect()
    }
}

impl TryFrom<String> for Config {
    type Error = anyhow::Error;

    fn try_from(contents: String) -> Result<Self, Self::Error> {
        let d: ConfigDeser = kdl::de::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("failed to parse {CONFIG_FILE_NAME}: {e}"))?;
        Ok(Config {
            template: d.template.unwrap_or_default(),
            placeholders: d.placeholders,
            hooks: d.hooks,
            conditional: d.conditional,
        })
    }
}

impl Config {
    /// Read a config from `path`, or `Config::default()` if it is `None` / missing.
    pub fn from_path(path: &Option<impl AsRef<Path>>) -> Result<Self> {
        let config = match path {
            Some(path) => match fs::read_to_string(path) {
                Ok(contents) => Self::try_from(contents)?,
                Err(e) => match e.kind() {
                    ErrorKind::NotFound => Self::default(),
                    _ => bail!(e),
                },
            },
            None => Self::default(),
        };
        Ok(config)
    }

    /// Hook commands for a given phase (empty if no `hooks` section).
    pub fn hooks_for(&self, phase: HookPhase) -> Vec<String> {
        self.hooks
            .as_ref()
            .map(|h| h.get(phase).to_vec())
            .unwrap_or_default()
    }

    /// Every hook command in execution order, across all phases.
    pub fn all_hooks(&self) -> Vec<String> {
        self.hooks
            .as_ref()
            .map(|h| h.all().into_iter().map(str::to_owned).collect())
            .unwrap_or_default()
    }
}

/// Search a folder tree for template configuration files, but look no deeper
/// than a found file. Ported verbatim from cargo-generate (format-agnostic).
pub fn locate_template_configs(base_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut results = Vec::with_capacity(1);

    if base_dir.is_dir() {
        let mut paths_to_search_in = vec![base_dir.to_path_buf()];
        'next_path: while let Some(path) = paths_to_search_in.pop() {
            let mut sub_paths: Vec<PathBuf> = vec![];
            for entry in fs::read_dir(&path)? {
                let entry = entry?;
                let entry_path = entry.path();
                if entry_path.is_dir() {
                    sub_paths.push(entry_path);
                } else if entry.file_name() == CONFIG_FILE_NAME {
                    results.push(path.strip_prefix(base_dir)?.to_path_buf());
                    continue 'next_path;
                }
            }
            paths_to_search_in.append(&mut sub_paths);
        }
    } else {
        results.push(base_dir.to_path_buf());
    }

    results.sort();
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::path::Path;
    use std::str::FromStr;
    use tempfile::TempDir;

    fn tmp_dir() -> TempDir {
        tempfile::Builder::new()
            .prefix("openeis-generate")
            .tempdir()
            .expect("failed to create temp dir")
    }

    fn create_file(base: &TempDir, path: impl AsRef<Path>, contents: impl AsRef<str>) {
        let path = base.path().join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dirs");
        }
        File::create(&path)
            .expect("create file")
            .write_all(contents.as_ref().as_bytes())
            .expect("write file");
    }

    #[test]
    fn deserializes_full_config() {
        let kdl = r#"
template {
    generator_version ">=0.8.0"
    include "Cargo.toml" "README.md"
    exclude "target"
    vcs "Git"
    init #false
}
placeholders {
    author {
        type "string"
        prompt "Author?"
        default "Alice"
    }
    use_std {
        type "bool"
        prompt "Use std?"
        default #true
    }
}
hooks {
    init "echo hi"
    pre "cargo fmt"
    post "cargo test"
}
conditional {
    "crate_type == \"lib\"" {
        ignore "src/bin"
    }
}
"#;
        let cfg = Config::try_from(kdl.to_string()).expect("parse");

        assert_eq!(
            cfg.template,
            TemplateConfig {
                sub_templates: None,
                generator_version: Some(VersionReq::from_str(">=0.8.0").unwrap()),
                include: Some(vec!["Cargo.toml".into(), "README.md".into()]),
                exclude: Some(vec!["target".into()]),
                ignore: None,
                vcs: Some(Vcs::Git),
                init: Some(false),
            }
        );

        let placeholders = cfg.placeholders.as_ref().expect("placeholders");
        assert_eq!(placeholders.len(), 2);
        let author = placeholders.get("author").expect("author");
        assert!(author.is_string());
        assert_eq!(author.prompt, "Author?");
        assert_eq!(author.default.as_ref().unwrap(), &serde_json::json!("Alice"));
        let use_std = placeholders.get("use_std").expect("use_std");
        assert!(use_std.is_bool());
        assert_eq!(use_std.default.as_ref().unwrap(), &serde_json::json!(true));

        assert_eq!(cfg.all_hooks(), vec!["echo hi", "cargo fmt", "cargo test"]);
        assert_eq!(cfg.hooks_for(HookPhase::Pre), vec!["cargo fmt"]);

        let cond = cfg.conditional.expect("conditional");
        assert_eq!(
            cond.get(r#"crate_type == "lib""#)
                .expect("lib condition")
                .ignore,
            Some(vec!["src/bin".to_string()])
        );
    }

    #[test]
    fn conditional_hash_string_key_avoids_escaping() {
        // The KDL hash-string #"..."# lets the rhai expression keep its inner
        // double-quotes unescaped. kdl 6.7.1 parses it to the same key the
        // escaped form `"database != \"sqlite\""` would produce.
        let kdl = r##"
conditional {
    #"database != "sqlite""# {
        ignore "database/**"
    }
}
"##;
        let cfg = Config::try_from(kdl.to_string()).expect("parse");
        let cond = cfg.conditional.expect("conditional");
        let c = cond
            .get("database != \"sqlite\"")
            .expect("hash-string key parsed unescaped");
        assert_eq!(c.ignore, Some(vec!["database/**".to_string()]));
    }

    #[test]
    fn try_from_handles_empty() {
        let cfg = Config::try_from(String::new()).expect("empty parses");
        assert_eq!(cfg.template, TemplateConfig::default());
        assert!(cfg.placeholders.is_none());
        assert!(cfg.hooks.is_none());
        assert!(cfg.conditional.is_none());
    }

    #[test]
    fn init_bool_forms() {
        // Booleans are written `#true`/`#false` (kdl 6.7.1 v2 treats bare
        // true/false as identifiers). Both inline and own-line forms parse.
        let inline = Config::try_from("template { init #false }".to_string()).expect("parse");
        assert_eq!(inline.template.init, Some(false));

        let own_line =
            Config::try_from("template {\n    init #true\n}".to_string()).expect("parse");
        assert_eq!(own_line.template.init, Some(true));

        // `template { vcs "Git" }` (no init) leaves init as None.
        let no_init = Config::try_from(r#"template { vcs "Git" }"#.to_string()).expect("parse");
        assert_eq!(no_init.template.init, None);
        assert_eq!(no_init.template.vcs, Some(Vcs::Git));
    }

    #[test]
    fn bool_placeholder_and_hash_string_conditional_coexist() {
        // Regression: with bare bools we needed v1-fallback, which then rejected
        // hash-strings. Now bools use #false (pure v2) so a hash-string
        // conditional key works in the SAME document as a bool placeholder.
        let kdl = r##"
placeholders {
    spa {
        type "bool"
        prompt "spa?"
        default #false
    }
}
conditional {
    #"database != "sqlite""# {
        ignore "database/**"
    }
}
"##;
        let cfg = Config::try_from(kdl.to_string()).expect("parse");
        let spa = cfg.placeholders.as_ref().unwrap().get("spa").unwrap();
        assert_eq!(spa.default.as_ref().unwrap(), &serde_json::json!(false));
        let key = cfg
            .conditional
            .as_ref()
            .unwrap()
            .get("database != \"sqlite\"")
            .expect("hash-string key");
        assert_eq!(key.ignore, Some(vec!["database/**".to_string()]));
    }

    #[test]
    fn from_path_returns_default_when_missing() {
        let tmp = tmp_dir();
        let missing = tmp.path().join("nope.kdl");
        let cfg = Config::from_path(&Some(missing)).expect("ok");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn from_path_reads_existing_file() {
        let tmp = tmp_dir();
        create_file(&tmp, CONFIG_FILE_NAME, "template { vcs \"Git\" }");
        let path = tmp.path().join(CONFIG_FILE_NAME);
        let cfg = Config::from_path(&Some(path)).expect("ok");
        assert_eq!(cfg.template.vcs, Some(Vcs::Git));
    }

    #[test]
    fn locate_configs_finds_files_and_stops_at_first_match() {
        let tmp = tmp_dir();
        create_file(&tmp, "dir1/Cargo.toml", "");
        create_file(&tmp, "dir2/dir2_2/template.kdl", "");
        create_file(&tmp, "dir3/Cargo.toml", "");
        create_file(&tmp, "dir4/template.kdl", "");

        let expected = vec![Path::new("dir2").join("dir2_2"), PathBuf::from("dir4")];
        let result = locate_template_configs(tmp.path()).expect("ok");
        assert_eq!(result, expected);
    }

    #[test]
    fn locate_configs_returns_empty_when_none() {
        let tmp = tmp_dir();
        create_file(&tmp, "dir1/Cargo.toml", "");
        let result = locate_template_configs(tmp.path()).expect("ok");
        assert!(result.is_empty());
    }
}
