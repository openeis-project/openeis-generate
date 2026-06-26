//! Template variable collection — ported (focused) from cargo-generate's
//! `project_variables.rs` + `template_values.rs`.
//!
//! Supports `bool` and `string` placeholders with `default`, `choices`, and
//! `regex` validation. Values come from `--define foo=bar` (highest priority),
//! then interactive prompts (via `dialoguer`), with `--silent` requiring every
//! placeholder to be resolvable without prompting.
//!
//! The decision logic ([`resolve`]) is kept free of I/O so it can be unit-tested;
//! only the thin [`prompt`] helpers touch `dialoguer`.

use anyhow::{Context, Result};
use heck::ToSnakeCase;
use indexmap::IndexMap;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::path::Path;
use std::process::Command;
use thiserror::Error;

use crate::config::{Config, Placeholder};

/// Reserved variable names supplied by the generator itself; placeholders may
/// not claim them.
pub const RESERVED_NAMES: &[&str] = &[
    "authors",
    "os-arch",
    "project-name",
    "crate_name",
    "crate_type",
    "within_cargo_project",
    "is_init",
    "username",
];

/// Resolved template variables, all stringified (liquid consumes strings).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Variables(pub IndexMap<String, String>);

impl Variables {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.0.iter()
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum VariableError {
    #[error("placeholder `{0}` claims a reserved name")]
    ReservedName(String),
    #[error("missing prompt for placeholder `{0}`")]
    MissingPrompt(String),
    #[error("invalid placeholder type `{value}` for `{var_name}` (expected `bool`, `string`, or `array`)")]
    InvalidType { var_name: String, value: String },
    #[error("default for array placeholder `{var_name}` must be a list, got `{value}`")]
    InvalidArrayDefault { var_name: String, value: String },
    #[error("default `{default}` for `{var_name}` is not one of choices {choices:?}")]
    InvalidDefault {
        var_name: String,
        default: String,
        choices: Vec<String>,
    },
    #[error("value `{value}` for `{var_name}` is not one of choices {choices:?}")]
    InvalidChoice {
        var_name: String,
        value: String,
        choices: Vec<String>,
    },
    #[error("value `{value}` for `{var_name}` does not match regex `{regex}`")]
    RegexNoMatch {
        var_name: String,
        value: String,
        regex: String,
    },
    #[error("bool placeholder `{0}` expects `true`/`false`, got `{1}`")]
    InvalidBool(String, String),
    #[error("regex of `{var_name}` is invalid: {error}")]
    InvalidRegex { var_name: String, error: String },
    #[error(
        "variable `{0}` is missing a default value and none was supplied via --define (silent mode)"
    )]
    MissingSilent(String),
}

/// Parse `--define foo=bar` entries into an ordered map. Errors if an entry
/// has no `=` or an empty key.
pub fn parse_defines(defines: &[String]) -> Result<IndexMap<String, String>> {
    let mut map = IndexMap::new();
    for entry in defines {
        let (k, v) = entry
            .split_once('=')
            .with_context(|| format!("--define expects KEY=VALUE, got `{entry}`"))?;
        let k = k.trim();
        if k.is_empty() {
            anyhow::bail!("--define has empty key in `{entry}`");
        }
        map.insert(k.to_string(), v.to_string());
    }
    Ok(map)
}

/// Load template variables from a KDL `--values-file` of the form
/// `values { key "value" … }`. Each value is stringified to match how resolved
/// variables are stored: scalars via [`json_to_string`], and arrays (e.g.
/// `features "auth" "logging"`) joined with commas so they feed an array
/// placeholder the same way a comma-joined `--define` does.
pub fn load_values_file(path: &Path) -> Result<IndexMap<String, String>> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading values file {}", path.display()))?;

    #[derive(Deserialize)]
    struct Wrapper {
        values: Option<IndexMap<String, JsonValue>>,
    }
    let w: Wrapper = kdl::de::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("failed to parse values file {}: {e}", path.display()))?;

    let mut out = IndexMap::new();
    for (k, v) in w.values.unwrap_or_default() {
        out.insert(k, value_file_to_string(&v));
    }
    Ok(out)
}

/// Stringify a `--values-file` entry. Arrays are comma-joined (so an array
/// placeholder receives `"a,b"` like a comma-separated `--define`); everything
/// else uses [`json_to_string`].
fn value_file_to_string(v: &JsonValue) -> String {
    match v {
        JsonValue::Array(arr) => arr.iter().map(json_to_string).collect::<Vec<_>>().join(","),
        other => json_to_string(other),
    }
}

/// What to do with one placeholder, given any value supplied via `--define`
/// (`provided`) and whether we may prompt (`silent` = no prompting allowed).
//
// Note: can't derive PartialEq/Eq — the `PromptString` variant carries a
// `regex::Regex`, which implements neither.
#[derive(Debug, Clone)]
pub enum Resolved {
    /// A final value (from `--define` or a default used in silent mode).
    Value(String),
    /// Needs an interactive string prompt.
    PromptString {
        default: Option<String>,
        choices: Vec<String>,
        regex: Option<Regex>,
    },
    /// Needs an interactive bool prompt.
    PromptBool {
        default: Option<bool>,
    },
    /// Needs an interactive multi-select (array) prompt. `default` are the
    /// pre-selected entries; `choices` is the selectable set (possibly empty,
    /// in which case the prompt falls back to free-form comma input).
    PromptArray {
        default: Vec<String>,
        choices: Vec<String>,
    },
}

impl Resolved {
    /// The final value, if this is [`Resolved::Value`].
    pub fn value(&self) -> Option<&str> {
        match self {
            Resolved::Value(v) => Some(v),
            _ => None,
        }
    }
}

/// Resolve a single placeholder to a [`Resolved`] (the non-I/O decision).
pub fn resolve(
    name: &str,
    p: &Placeholder,
    provided: Option<&str>,
    silent: bool,
) -> Result<Resolved, VariableError> {
    if RESERVED_NAMES.contains(&name) {
        return Err(VariableError::ReservedName(name.into()));
    }
    if p.prompt.trim().is_empty() {
        return Err(VariableError::MissingPrompt(name.into()));
    }

    let choices: Vec<String> = p.choices.iter().flatten().map(json_to_string).collect();

    if p.is_bool() {
        return resolve_bool(name, p, provided, silent);
    }
    if p.is_array() {
        return resolve_array(name, p, provided, silent, &choices);
    }
    if !p.is_string() {
        return Err(VariableError::InvalidType {
            var_name: name.into(),
            value: p.r#type.clone(),
        });
    }
    resolve_string(name, p, provided, silent, &choices)
}

/// Bool branch: ignores `choices`/`regex`. `--define` normalizes; silent uses
/// the default; otherwise an interactive confirm is requested.
fn resolve_bool(
    name: &str,
    p: &Placeholder,
    provided: Option<&str>,
    silent: bool,
) -> Result<Resolved, VariableError> {
    if let Some(raw) = provided {
        let normalized = normalize_bool(raw)
            .ok_or_else(|| VariableError::InvalidBool(name.into(), raw.into()))?;
        return Ok(Resolved::Value(normalized));
    }
    let default = p.default.as_ref().and_then(JsonValue::as_bool);
    if silent {
        default
            .map(|b| Resolved::Value(b.to_string()))
            .ok_or_else(|| VariableError::MissingSilent(name.into()))
    } else {
        Ok(Resolved::PromptBool { default })
    }
}

/// String branch: `choices` (single-select) and `regex` validation apply.
fn resolve_string(
    name: &str,
    p: &Placeholder,
    provided: Option<&str>,
    silent: bool,
    choices: &[String],
) -> Result<Resolved, VariableError> {
    let regex = match p.regex.as_ref() {
        Some(pattern) => Some(Regex::new(pattern).map_err(|e| VariableError::InvalidRegex {
            var_name: name.into(),
            error: e.to_string(),
        })?),
        None => None,
    };

    // Validate a default against choices up-front.
    if let Some(default) = p.default.as_ref() {
        let default_str = json_to_string(default);
        if !choices.is_empty() && !choices.contains(&default_str) {
            return Err(VariableError::InvalidDefault {
                var_name: name.into(),
                default: default_str,
                choices: choices.to_vec(),
            });
        }
    }

    // Explicit --define wins (after validation).
    if let Some(raw) = provided {
        validate_string(name, raw, choices, regex.as_ref())?;
        return Ok(Resolved::Value(raw.to_string()));
    }

    let default = p.default.as_ref().map(json_to_string);
    if silent {
        return default
            .map(Resolved::Value)
            .ok_or_else(|| VariableError::MissingSilent(name.into()));
    }
    Ok(Resolved::PromptString {
        default,
        choices: choices.to_vec(),
        regex,
    })
}

/// Array branch (multi-select). The resolved value is a comma-joined string.
///
/// KDL ambiguity note: a single-argument `default "x"` deserializes to a JSON
/// scalar string while `default "x" "y"` becomes an array — both are accepted
/// (the scalar is treated as a one-element default). `--define` is parsed as a
/// comma-separated list.
fn resolve_array(
    name: &str,
    p: &Placeholder,
    provided: Option<&str>,
    silent: bool,
    choices: &[String],
) -> Result<Resolved, VariableError> {
    let default: Vec<String> = match p.default.as_ref() {
        None => Vec::new(),
        Some(JsonValue::Array(arr)) => arr.iter().map(json_to_string).collect(),
        Some(JsonValue::String(s)) => vec![s.clone()],
        Some(other) => {
            return Err(VariableError::InvalidArrayDefault {
                var_name: name.into(),
                value: other.to_string(),
            })
        }
    };

    // Every default entry must be among the choices (when choices constrain it).
    if !choices.is_empty() {
        for d in &default {
            if !choices.contains(d) {
                return Err(VariableError::InvalidDefault {
                    var_name: name.into(),
                    default: d.clone(),
                    choices: choices.to_vec(),
                });
            }
        }
    }

    if let Some(raw) = provided {
        let vals: Vec<String> = raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !choices.is_empty() {
            for v in &vals {
                if !choices.contains(v) {
                    return Err(VariableError::InvalidChoice {
                        var_name: name.into(),
                        value: v.clone(),
                        choices: choices.to_vec(),
                    });
                }
            }
        }
        return Ok(Resolved::Value(vals.join(",")));
    }

    if silent {
        return if default.is_empty() {
            Err(VariableError::MissingSilent(name.into()))
        } else {
            Ok(Resolved::Value(default.join(",")))
        };
    }

    Ok(Resolved::PromptArray {
        default,
        choices: choices.to_vec(),
    })
}

fn validate_string(
    name: &str,
    value: &str,
    choices: &[String],
    regex: Option<&Regex>,
) -> Result<(), VariableError> {
    if !choices.is_empty() && !choices.iter().any(|c| c == value) {
        return Err(VariableError::InvalidChoice {
            var_name: name.into(),
            value: value.into(),
            choices: choices.to_vec(),
        });
    }
    if let Some(re) = regex
        && !re.is_match(value)
    {
        return Err(VariableError::RegexNoMatch {
            var_name: name.into(),
            value: value.into(),
            regex: re.as_str().into(),
        });
    }
    Ok(())
}

fn normalize_bool(raw: &str) -> Option<String> {
    match raw.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Some("true".into()),
        "false" | "no" | "0" => Some("false".into()),
        _ => None,
    }
}

fn json_to_string(v: &JsonValue) -> String {
    match v {
        JsonValue::String(s) => s.clone(),
        JsonValue::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Collect every placeholder's value, prompting interactively when needed.
///
/// In `silent` mode no prompting happens: every placeholder must resolve from
/// `defines` or its own default, else this returns an error.
pub fn collect_variables(
    config: &Config,
    defines: &IndexMap<String, String>,
    silent: bool,
) -> Result<Variables> {
    let mut vars = Variables::default();
    if let Some(placeholders) = config.placeholders.as_ref() {
        resolve_placeholders_into(placeholders, defines, silent, &mut vars)?;
    }
    Ok(vars)
}

/// Resolve every placeholder in `placeholders` that isn't already in `vars`,
/// prompting (or using define/default/silent rules) as needed, and insert into
/// `vars`. Used by both direct collection and the conditional merge loop.
pub fn resolve_placeholders_into(
    placeholders: &IndexMap<String, Placeholder>,
    defines: &IndexMap<String, String>,
    silent: bool,
    vars: &mut Variables,
) -> Result<()> {
    for (name, p) in placeholders {
        if vars.0.contains_key(name) {
            continue;
        }
        let provided = defines.get(name).map(String::as_str);
        match resolve(name, p, provided, silent).map_err(anyhow::Error::from)? {
            Resolved::Value(v) => {
                vars.0.insert(name.clone(), v);
            }
            Resolved::PromptString {
                default,
                choices,
                regex,
            } => {
                let v = prompt_string(&p.prompt, default.as_deref(), &choices, regex.as_ref())
                    .with_context(|| format!("prompting for `{name}`"))?;
                vars.0.insert(name.clone(), v);
            }
            Resolved::PromptBool { default } => {
                let b = prompt_bool(&p.prompt, default.unwrap_or(false))
                    .with_context(|| format!("prompting for `{name}`"))?;
                vars.0.insert(name.clone(), b.to_string());
            }
            Resolved::PromptArray { default, choices } => {
                let v = prompt_array(&p.prompt, &default, &choices)
                    .with_context(|| format!("prompting for `{name}`"))?;
                vars.0.insert(name.clone(), v);
            }
        }
    }
    Ok(())
}

// --- dialoguer I/O (thin, untested) -----------------------------------------

fn prompt_string(
    prompt: &str,
    default: Option<&str>,
    choices: &[String],
    regex: Option<&Regex>,
) -> Result<String> {
    use dialoguer::Select;
    if !choices.is_empty() {
        let default_idx = default
            .and_then(|d| choices.iter().position(|c| c == d))
            .unwrap_or(0);
        let idx = Select::new()
            .with_prompt(prompt)
            .items(choices)
            .default(default_idx)
            .interact_opt()?
            .unwrap_or(default_idx);
        return Ok(choices[idx].clone());
    }

    use dialoguer::Input;
    let mut input: Input<String> = Input::new().with_prompt(prompt);
    if let Some(d) = default {
        input = input.default(d.to_string());
    }
    if let Some(re) = regex {
        let re = re.clone();
        input = input.validate_with(move |s: &String| {
            re.is_match(s)
                .then_some(())
                .ok_or_else(|| format!("must match `{}`", re.as_str()))
        });
    }
    Ok(input.interact_text()?)
}

fn prompt_bool(prompt: &str, default: bool) -> Result<bool> {
    use dialoguer::Confirm;
    Ok(Confirm::new()
        .with_prompt(prompt)
        .default(default)
        .interact()?)
}

/// Multi-select prompt for an array placeholder. The selection is returned as
/// a comma-joined string. With no `choices` the prompt falls back to free-form
/// comma-separated text input.
fn prompt_array(prompt: &str, default: &[String], choices: &[String]) -> Result<String> {
    if choices.is_empty() {
        use dialoguer::Input;
        let pre = default.join(",");
        let mut input: Input<String> = Input::new().with_prompt(prompt);
        if !pre.is_empty() {
            input = input.default(pre);
        }
        let raw: String = input.interact_text()?;
        return Ok(raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(","));
    }

    use dialoguer::MultiSelect;
    let defaults: Vec<bool> = choices.iter().map(|c| default.contains(c)).collect();
    let selected = MultiSelect::new()
        .with_prompt(prompt)
        .items(choices)
        .defaults(&defaults)
        .interact()?;
    Ok(selected
        .iter()
        .map(|&i| choices[i].clone())
        .collect::<Vec<_>>()
        .join(","))
}

// --- built-in variables -----------------------------------------------------

/// Seed the generator-supplied built-in variables (the [`RESERVED_NAMES`]) into
/// `vars`, mirroring cargo-generate's always-available variables so templates
/// can reference `{{ crate_name }}`, `{{ authors }}`, `{{ os-arch }}`, etc.
/// without a placeholder. Existing entries are left untouched, so a pre-hook or
/// `--define` may still influence them (placeholders themselves can't claim
/// reserved names — see [`resolve`]).
///
/// `name` is the `--name` value (drives `name`, `project-name`, `crate_name`);
/// `is_init` is whether `--init` was passed.
pub fn seed_builtins(vars: &mut Variables, name: Option<&str>, is_init: bool) {
    let map = &mut vars.0;
    if let Some(n) = name {
        map.entry("name".into()).or_insert(n.to_string());
        map.entry("project-name".into()).or_insert(n.to_string());
        map.entry("crate_name".into()).or_insert(n.to_snake_case());
    }
    // No --lib/--bin flag (cargo-generate derives crate_type from those);
    // default to "bin" so `crate_type`-keyed conditionals resolve.
    map.entry("crate_type".into()).or_insert("bin".to_string());
    map.entry("os-arch".into())
        .or_insert(format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH));
    map.entry("is_init".into()).or_insert(is_init.to_string());
    map.entry("within_cargo_project".into())
        .or_insert(within_cargo_project().to_string());

    if let Some((author, username)) = discover_author() {
        map.entry("authors".into()).or_insert(author);
        map.entry("username".into()).or_insert(username);
    }
}

/// Best-effort author discovery, mirroring cargo-generate (itself taken from
/// `cargo new`). Unlike cargo-generate we read `git config` through the external
/// git binary instead of libgit2, consistent with the rest of this crate.
/// Returns `(authors_string, username)` or `None` when no name is discoverable.
fn discover_author() -> Option<(String, String)> {
    fn env_of(vars: &[&str]) -> Option<String> {
        vars.iter().filter_map(|v| std::env::var(v).ok()).next()
    }

    let name = env_of(&["CARGO_NAME", "GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"])
        .or_else(|| git_config("user.name"))
        .or_else(|| env_of(&["USER", "USERNAME", "NAME"]))?;
    let email = env_of(&["CARGO_EMAIL", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"])
        .or_else(|| git_config("user.email"))
        .or_else(|| std::env::var("EMAIL").ok());

    let name = name.trim().to_string();
    let email = email.map(|e| e.trim().trim_start_matches('<').trim_end_matches('>').to_string());
    let author = match email {
        Some(email) if !email.is_empty() => format!("{name} <{email}>"),
        _ => name.clone(),
    };
    Some((author, name))
}

/// Read a value from `git config` (best-effort; errors / missing → `None`).
fn git_config(key: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--get", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// True if any ancestor of the current directory contains a `Cargo.toml`
/// (i.e. we appear to be generating inside an existing Cargo workspace/project).
fn within_cargo_project() -> bool {
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        if d.join("Cargo.toml").exists() {
            return true;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Placeholder;

    fn str_placeholder(prompt: &str) -> Placeholder {
        Placeholder {
            r#type: "string".into(),
            prompt: prompt.into(),
            default: None,
            choices: None,
            regex: None,
        }
    }

    #[test]
    fn parse_defines_basic_and_rejects_bad() {
        let m = parse_defines(&["a=1".into(), "b=2".into()]).unwrap();
        assert_eq!(m.get("a").map(String::as_str), Some("1"));
        assert_eq!(m.get("b").map(String::as_str), Some("2"));

        assert!(parse_defines(&["nope".into()]).is_err()); // missing '='
        assert!(parse_defines(&["=val".into()]).is_err()); // empty key
        // value may be empty:
        assert_eq!(
            parse_defines(&["k=".into()]).unwrap().get("k").map(String::as_str),
            Some("")
        );
    }

    #[test]
    fn define_overrides_everything() {
        let p = Placeholder {
            r#type: "string".into(),
            prompt: "x".into(),
            default: Some(JsonValue::String("def".into())),
            choices: None,
            regex: None,
        };
        let r = resolve("v", &p, Some("custom"), false).unwrap();
        assert_eq!(r.value(), Some("custom"));
    }

    #[test]
    fn bool_define_normalizes() {
        let p = Placeholder {
            r#type: "bool".into(),
            prompt: "x".into(),
            default: None,
            choices: None,
            regex: None,
        };
        assert_eq!(
            resolve("v", &p, Some("yes"), false).unwrap().value(),
            Some("true")
        );
        assert!(matches!(
            resolve("v", &p, Some("maybe"), false),
            Err(VariableError::InvalidBool(_, _))
        ));
    }

    #[test]
    fn silent_uses_default_or_errors() {
        let with_default = Placeholder {
            r#type: "bool".into(),
            prompt: "x".into(),
            default: Some(JsonValue::Bool(true)),
            choices: None,
            regex: None,
        };
        assert_eq!(
            resolve("v", &with_default, None, true).unwrap().value(),
            Some("true")
        );

        let no_default = str_placeholder("x");
        assert_eq!(
            resolve("v", &no_default, None, true).unwrap_err(),
            VariableError::MissingSilent("v".into())
        );
    }

    #[test]
    fn non_silent_requests_prompt() {
        let p = str_placeholder("your name?");
        assert!(matches!(
            resolve("v", &p, None, false).unwrap(),
            Resolved::PromptString { default: None, .. }
        ));

        let with_default = Placeholder {
            r#type: "string".into(),
            prompt: "x".into(),
            default: Some(JsonValue::String("d".into())),
            choices: None,
            regex: None,
        };
        assert!(matches!(
            resolve("v", &with_default, None, false).unwrap(),
            Resolved::PromptString { default: Some(_), .. }
        ));
    }

    #[test]
    fn choices_validate_default_and_value() {
        let p = Placeholder {
            r#type: "string".into(),
            prompt: "x".into(),
            default: Some(JsonValue::String("red".into())), // not in choices
            choices: Some(vec![JsonValue::String("a".into()), JsonValue::String("b".into())]),
            regex: None,
        };
        assert!(matches!(
            resolve("v", &p, None, false),
            Err(VariableError::InvalidDefault { .. })
        ));

        let p_ok = Placeholder {
            r#type: "string".into(),
            prompt: "x".into(),
            default: None,
            choices: Some(vec![JsonValue::String("a".into()), JsonValue::String("b".into())]),
            regex: None,
        };
        assert!(matches!(
            resolve("v", &p_ok, Some("c"), false),
            Err(VariableError::InvalidChoice { .. })
        ));
        assert_eq!(
            resolve("v", &p_ok, Some("a"), false).unwrap().value(),
            Some("a")
        );
    }

    #[test]
    fn regex_validates_and_compiles() {
        let p = Placeholder {
            r#type: "string".into(),
            prompt: "x".into(),
            default: None,
            choices: None,
            regex: Some("^[a-z]+$".into()),
        };
        assert!(matches!(
            resolve("v", &p, Some("ABC"), false),
            Err(VariableError::RegexNoMatch { .. })
        ));
        assert_eq!(
            resolve("v", &p, Some("abc"), false).unwrap().value(),
            Some("abc")
        );

        let bad = Placeholder {
            regex: Some("[".into()),
            ..str_placeholder("x")
        };
        assert!(matches!(
            resolve("v", &bad, None, false),
            Err(VariableError::InvalidRegex { .. })
        ));
    }

    #[test]
    fn reserved_and_missing_prompt_rejected() {
        let p = str_placeholder("x");
        assert_eq!(
            resolve("crate_type", &p, None, false).unwrap_err(),
            VariableError::ReservedName("crate_type".into())
        );
        // username is now a reserved built-in too.
        assert_eq!(
            resolve("username", &p, None, false).unwrap_err(),
            VariableError::ReservedName("username".into())
        );

        let no_prompt = Placeholder {
            prompt: "  ".into(),
            ..str_placeholder("x")
        };
        assert!(matches!(
            resolve("v", &no_prompt, None, false),
            Err(VariableError::MissingPrompt(_))
        ));
    }

    #[test]
    fn seed_builtins_populates_expected_keys() {
        let mut v = Variables::default();
        seed_builtins(&mut v, Some("My Project"), true);

        assert_eq!(v.get("name"), Some("My Project"));
        assert_eq!(v.get("project-name"), Some("My Project"));
        assert_eq!(v.get("crate_name"), Some("my_project")); // snake_case of the name
        assert_eq!(v.get("crate_type"), Some("bin"));
        assert_eq!(v.get("is_init"), Some("true"));
        // os-arch is "<os>-<arch>"; just assert it's set and shaped.
        let os_arch = v.get("os-arch").unwrap();
        assert!(os_arch.contains('-'), "os-arch = {os_arch:?}");
        assert!(v.get("within_cargo_project").is_some());
        // authors/username resolve from the environment in CI; assert shape only.
        if let Some(authors) = v.get("authors") {
            assert!(v.get("username").is_some(), "username set when authors is");
            let _ = authors; // e.g. "Name <email>"
        }
    }

    #[test]
    fn seed_builtins_without_name_omits_name_keys() {
        let mut v = Variables::default();
        seed_builtins(&mut v, None, false);
        assert!(v.get("name").is_none());
        assert!(v.get("project-name").is_none());
        assert!(v.get("crate_name").is_none());
        // non-name built-ins are still set.
        assert_eq!(v.get("crate_type"), Some("bin"));
        assert_eq!(v.get("is_init"), Some("false"));
    }

    fn array_placeholder(default: Option<JsonValue>) -> Placeholder {
        Placeholder {
            r#type: "array".into(),
            prompt: "Features?".into(),
            default,
            choices: Some(vec![
                JsonValue::String("auth".into()),
                JsonValue::String("logging".into()),
                JsonValue::String("metrics".into()),
            ]),
            regex: None,
        }
    }

    #[test]
    fn array_define_is_comma_split_and_validated() {
        let p = array_placeholder(None);
        // comma-separated --define, all valid
        assert_eq!(
            resolve("feat", &p, Some("auth, metrics"), false).unwrap().value(),
            Some("auth,metrics")
        );
        // unknown choice rejected
        assert!(matches!(
            resolve("feat", &p, Some("auth,nope"), false),
            Err(VariableError::InvalidChoice { .. })
        ));
    }

    #[test]
    fn array_default_accepts_scalar_and_list_forms() {
        // KDL `default "auth"` → scalar; treated as a one-element default.
        let p = array_placeholder(Some(JsonValue::String("auth".into())));
        let r = resolve("feat", &p, None, true).unwrap(); // silent → uses default
        assert_eq!(r.value(), Some("auth"));

        // KDL `default "auth" "logging"` → array.
        let p = array_placeholder(Some(JsonValue::Array(vec![
            JsonValue::String("auth".into()),
            JsonValue::String("logging".into()),
        ])));
        assert_eq!(
            resolve("feat", &p, None, true).unwrap().value(),
            Some("auth,logging")
        );

        // default not in choices → InvalidDefault.
        let p = array_placeholder(Some(JsonValue::String("nope".into())));
        assert!(matches!(
            resolve("feat", &p, None, true),
            Err(VariableError::InvalidDefault { .. })
        ));
    }

    #[test]
    fn array_default_wrong_type_errors() {
        // A bool default for an array placeholder is rejected.
        let p = array_placeholder(Some(JsonValue::Bool(true)));
        assert!(matches!(
            resolve("feat", &p, None, true),
            Err(VariableError::InvalidArrayDefault { .. })
        ));
    }

    #[test]
    fn array_silent_without_default_errors() {
        let p = array_placeholder(None);
        assert!(matches!(
            resolve("feat", &p, None, true),
            Err(VariableError::MissingSilent(_))
        ));
    }

    #[test]
    fn array_non_silent_requests_prompt() {
        let p = array_placeholder(Some(JsonValue::String("auth".into())));
        assert!(matches!(
            resolve("feat", &p, None, false).unwrap(),
            Resolved::PromptArray { default, .. } if default == vec!["auth".to_string()]
        ));
    }

    #[test]
    fn load_values_file_parses_and_stringifies() {
        use std::fs;
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("values.kdl");
        fs::write(
            &path,
            "values {\n    author \"Alice\"\n    count \"3\"\n    features \"auth\" \"logging\"\n}\n",
        )
        .unwrap();

        let m = load_values_file(&path).unwrap();
        assert_eq!(m.get("author").map(String::as_str), Some("Alice"));
        assert_eq!(m.get("count").map(String::as_str), Some("3"));
        // array value → comma-joined
        assert_eq!(m.get("features").map(String::as_str), Some("auth,logging"));
    }
}
