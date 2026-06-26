//! Conditional placeholder/filter overrides.
//!
//! `conditional { "<rhai expr>" { include …; exclude …; ignore …; placeholders {…} } }`
//! blocks are merged into the effective config when their expression evaluates
//! true against the collected variables. Mirrors cargo-generate's
//! `fill_placeholders_and_merge_conditionals` loop: collect placeholders, merge
//! matching conditionals, and repeat while new placeholders keep appearing.

use anyhow::Result;
use indexmap::IndexMap;

use crate::config::{ConditionalConfig, Config};
use crate::generate::GenerationOptions;
use crate::hooks::eval_condition;
use crate::variables::{resolve_placeholders_into, Variables};

/// Collect base + conditional placeholders into `vars` (prompting as needed),
/// and merge matching conditionals' include/exclude/ignore into the returned
/// [`GenerationOptions`]. Built-in variables (`name`, `project-name`) are seeded
/// from `name` first, so conditionals can reference them.
pub fn collect(
    config: &Config,
    defines: &IndexMap<String, String>,
    silent: bool,
    name: Option<&str>,
) -> Result<(Variables, GenerationOptions)> {
    let mut vars = Variables::default();
    if let Some(n) = name {
        vars.0.entry("name".into()).or_insert_with(|| n.to_string());
        vars.0
            .entry("project-name".into())
            .or_insert_with(|| n.to_string());
    }

    let mut effective: IndexMap<String, crate::config::Placeholder> =
        config.placeholders.clone().unwrap_or_default();
    let mut opts = GenerationOptions::from_template(&config.template);
    // Cloned so we can `.take()` fields (merge each conditional at most once).
    let mut conditionals: IndexMap<String, ConditionalConfig> =
        config.conditional.clone().unwrap_or_default();

    loop {
        resolve_placeholders_into(&effective, defines, silent, &mut vars)?;

        let mut grew = false;
        for (expr, c) in conditionals.iter_mut() {
            if eval_condition(expr, &vars)? != Some(true) {
                continue;
            }
            if let Some(e) = c.include.take() {
                opts.include.extend(e);
            }
            if let Some(e) = c.exclude.take() {
                opts.exclude.extend(e);
            }
            if let Some(e) = c.ignore.take() {
                opts.ignore.extend(e);
            }
            if let Some(extra) = c.placeholders.take() {
                for (k, v) in extra {
                    // Only a genuinely new placeholder should keep the loop going.
                    if effective.insert(k, v).is_none() {
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }

    Ok((vars, opts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConditionalConfig, Placeholder, TemplateConfig};
    use serde_json::json;

    fn str_ph(prompt: &str, default: Option<&str>) -> Placeholder {
        Placeholder {
            r#type: "string".into(),
            prompt: prompt.into(),
            default: default.map(|d| json!(d)),
            choices: None,
            regex: None,
        }
    }

    fn config(
        base: Vec<(&str, Placeholder)>,
        conditionals: Vec<(&str, ConditionalConfig)>,
    ) -> Config {
        Config {
            template: TemplateConfig::default(),
            placeholders: Some(base.into_iter().map(|(k, v)| (k.into(), v)).collect()),
            hooks: None,
            conditional: Some(
                conditionals
                    .into_iter()
                    .map(|(k, v)| (k.into(), v))
                    .collect(),
            ),
        }
    }

    #[test]
    fn matching_conditional_merges_placeholders_and_filters() {
        let cfg = config(
            vec![("lang", str_ph("Language?", Some("rust")))],
            vec![(
                r#"lang == "rust""#,
                ConditionalConfig {
                    include: Some(vec!["src/*".into()]),
                    exclude: None,
                    ignore: None,
                    placeholders: Some(
                        [("edition", str_ph("Edition?", Some("2024")))]
                            .into_iter()
                            .map(|(k, v)| (k.into(), v))
                            .collect(),
                    ),
                },
            )],
        );

        let (vars, opts) = collect(&cfg, &IndexMap::new(), true, Some("app")).unwrap();
        assert_eq!(vars.get("lang"), Some("rust"));
        assert_eq!(vars.get("edition"), Some("2024"));
        assert_eq!(vars.get("project-name"), Some("app"));
        assert_eq!(opts.include, vec!["src/*".to_string()]);
    }

    #[test]
    fn non_matching_conditional_is_skipped() {
        let cfg = config(
            vec![("lang", str_ph("Language?", Some("go")))],
            vec![(
                r#"lang == "rust""#,
                ConditionalConfig {
                    include: Some(vec!["rust-only/*".into()]),
                    exclude: None,
                    ignore: None,
                    placeholders: Some(
                        [("edition", str_ph("Edition?", Some("2024")))]
                            .into_iter()
                            .map(|(k, v)| (k.into(), v))
                            .collect(),
                    ),
                },
            )],
        );

        let (vars, opts) = collect(&cfg, &IndexMap::new(), true, None).unwrap();
        assert_eq!(vars.get("lang"), Some("go"));
        assert!(vars.get("edition").is_none(), "conditional placeholder not added");
        assert!(opts.include.is_empty(), "conditional include not merged");
    }

    #[test]
    fn conditionals_chain_via_new_placeholders() {
        // base adds `a`; cond1 (always true) adds `b`; cond2 matches when b == "y".
        let cfg = config(
            vec![("a", str_ph("A?", Some("1")))],
            vec![
                (
                    "true",
                    ConditionalConfig {
                        include: None,
                        exclude: None,
                        ignore: None,
                        placeholders: Some(
                            [("b", str_ph("B?", Some("y")))]
                                .into_iter()
                                .map(|(k, v)| (k.into(), v))
                                .collect(),
                        ),
                    },
                ),
                (
                    r#"b == "y""#,
                    ConditionalConfig {
                        include: Some(vec!["chained/*".into()]),
                        exclude: None,
                        ignore: None,
                        placeholders: None,
                    },
                ),
            ],
        );

        let (vars, opts) = collect(&cfg, &IndexMap::new(), true, None).unwrap();
        assert_eq!(vars.get("b"), Some("y"));
        assert_eq!(opts.include, vec!["chained/*".to_string()]);
    }

    #[test]
    fn invalid_expression_errors() {
        let cfg = config(
            vec![("a", str_ph("A?", Some("1")))],
            vec![(
                "this is not valid rhai ==",
                ConditionalConfig::default(),
            )],
        );
        let res = collect(&cfg, &IndexMap::new(), true, None);
        assert!(res.is_err());
    }
}
