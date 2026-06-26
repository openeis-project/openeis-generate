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
use indexmap::IndexMap;
use regex::Regex;
use serde_json::Value as JsonValue;
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
    #[error("invalid placeholder type `{value}` for `{var_name}` (expected `bool` or `string`)")]
    InvalidType { var_name: String, value: String },
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

    let choices: Vec<String> = p
        .choices
        .iter()
        .flatten()
        .map(json_to_string)
        .collect();
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
                choices: choices.clone(),
            });
        }
    }

    // 1) Explicit --define wins (after validation).
    if let Some(raw) = provided {
        if p.is_bool() {
            let normalized = normalize_bool(raw)
                .ok_or_else(|| VariableError::InvalidBool(name.into(), raw.into()))?;
            return Ok(Resolved::Value(normalized));
        }
        validate_string(name, raw, &choices, regex.as_ref())?;
        return Ok(Resolved::Value(raw.to_string()));
    }

    // 2) Bool placeholder.
    if p.is_bool() {
        let default = p.default.as_ref().and_then(JsonValue::as_bool);
        return if silent {
            default
                .map(|b| Resolved::Value(b.to_string()))
                .ok_or_else(|| VariableError::MissingSilent(name.into()))
        } else {
            Ok(Resolved::PromptBool { default })
        };
    }

    // 3) String (or unknown) placeholder.
    if !p.is_string() {
        return Err(VariableError::InvalidType {
            var_name: name.into(),
            value: p.r#type.clone(),
        });
    }
    let default = p.default.as_ref().map(json_to_string);
    if silent {
        return default
            .map(Resolved::Value)
            .ok_or_else(|| VariableError::MissingSilent(name.into()));
    }
    Ok(Resolved::PromptString {
        default,
        choices,
        regex,
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

        let no_prompt = Placeholder {
            prompt: "  ".into(),
            ..str_placeholder("x")
        };
        assert!(matches!(
            resolve("v", &no_prompt, None, false),
            Err(VariableError::MissingPrompt(_))
        ));
    }
}
