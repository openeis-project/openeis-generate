//! Template expansion: copy a template directory into a destination, rendering
//! Liquid (`{{ var }}`) in file names and contents.
//!
//! Focused port of cargo-generate's `template.rs` + `copy.rs` + `filenames.rs`.
//! Filtering uses `globset` (cargo-generate uses gitignore-style `ignore`).

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use liquid::{
    model::{KString, Value},
    Object, Parser, ParserBuilder,
};
use walkdir::WalkDir;

use crate::config::TemplateConfig;
use crate::variables::Variables;

/// Files with this suffix are always rendered and the suffix is stripped from
/// the output name (`README.md.liquid` → `README.md`).
pub const LIQUID_SUFFIX: &str = ".liquid";

/// A template-root file whose lines are extra `ignore` globs (one per line,
/// `#` comments / blank lines skipped). Always excluded from the output itself.
pub const IGNORE_FILE_NAME: &str = ".genignore";

/// Paths that are never copied out of a template.
const NEVER_COPY: &[&str] = &[".git"];

/// What to render and what to skip.
#[derive(Debug, Clone, Default)]
pub struct GenerationOptions {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub ignore: Vec<String>,
}

impl GenerationOptions {
    pub fn from_template(t: &TemplateConfig) -> Self {
        Self {
            include: t.include.clone().unwrap_or_default(),
            exclude: t.exclude.clone().unwrap_or_default(),
            ignore: t.ignore.clone().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GenerationStats {
    pub dirs_created: usize,
    pub files_rendered: usize,
    pub files_copied: usize,
}

/// Render a single Liquid template string against the variables. Errors if the
/// template is syntactically invalid.
pub fn render_template(text: &str, vars: &Variables) -> Result<String> {
    let parser = build_parser()?;
    let object = to_object(vars);
    render_with(&parser, text, &object)
}

/// Build the Liquid parser with the case-conversion filters registered. Public
/// so tests can render with the same filter set the generator uses.
pub fn parser_with_filters() -> Parser {
    build_parser().expect("liquid parser with filters always builds")
}

/// Copy `template_dir` into `dest_dir`, rendering file names and contents.
pub fn expand(
    template_dir: &Path,
    dest_dir: &Path,
    vars: &Variables,
    opts: &GenerationOptions,
) -> Result<GenerationStats> {
    if !template_dir.is_dir() {
        anyhow::bail!("template path is not a directory: {}", template_dir.display());
    }

    let parser = build_parser()?;
    let object = to_object(vars);
    let include = build_globset(&opts.include)?;
    let exclude = build_globset(&opts.exclude)?;
    // The ignore set combines the config `ignore` globs with a template-root
    // `.genignore` file (if present).
    let mut ignore_patterns = opts.ignore.clone();
    ignore_patterns.extend(load_genignore(template_dir));
    let ignore = build_globset(&ignore_patterns)?;
    let mut stats = GenerationStats::default();

    for entry in WalkDir::new(template_dir)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| !is_excluded_dir(e.path(), template_dir))
    {
        let entry = entry?;
        let rel = entry.path().strip_prefix(template_dir)?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        // Never copy the config file, the .genignore file, or never-copy entries.
        if rel_str == crate::CONFIG_FILE_NAME
            || rel_str == IGNORE_FILE_NAME
            || never_copy(&rel_str)
        {
            continue;
        }

        if entry.file_type().is_dir() {
            let dest = dest_dir.join(render_relpath(rel, &parser, &object)?);
            std::fs::create_dir_all(&dest)
                .with_context(|| format!("creating dir {}", dest.display()))?;
            stats.dirs_created += 1;
            continue;
        }

        if entry.file_type().is_file() {
            // include whitelist (empty = allow all), then exclude/ignore.
            if !passes_filters(&rel_str, &include, &exclude, &ignore) {
                continue;
            }
            let dest = dest_dir.join(render_relpath(rel, &parser, &object)?);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            copy_or_render(entry.path(), &dest, &parser, &object, &mut stats)?;
        }
    }

    Ok(stats)
}

fn build_parser() -> Result<Parser> {
    Ok(ParserBuilder::with_stdlib()
        .filter(crate::template_filters::KebabCaseFilterParser)
        .filter(crate::template_filters::LowerCamelCaseFilterParser)
        .filter(crate::template_filters::PascalCaseFilterParser)
        .filter(crate::template_filters::ShoutyKebabCaseFilterParser)
        .filter(crate::template_filters::ShoutySnakeCaseFilterParser)
        .filter(crate::template_filters::SnakeCaseFilterParser)
        .filter(crate::template_filters::TitleCaseFilterParser)
        .filter(crate::template_filters::UpperCamelCaseFilterParser)
        .build()?)
}

pub(crate) fn to_object(vars: &Variables) -> Object {
    let mut o = Object::new();
    for (k, v) in vars.iter() {
        o.insert(KString::from_ref(k), Value::scalar(v.clone()));
    }
    o
}

fn render_with(parser: &Parser, text: &str, object: &Object) -> Result<String> {
    let template = parser.parse(text)?;
    Ok(template.render(object)?)
}

/// Render a path component. Liquid syntax errors in a name are treated as
/// literal (render the original) so a stray `{{` doesn't abort generation.
fn render_relpath(rel: &Path, parser: &Parser, object: &Object) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(s) => {
                let s = s.to_str().context("non-UTF8 path component in template")?;
                let base = s.strip_suffix(LIQUID_SUFFIX).unwrap_or(s);
                let rendered = render_graceful(base, parser, object);
                out.push(sanitize_filename::sanitize(&rendered));
            }
            other => out.push(other.as_os_str()),
        }
    }
    Ok(out)
}

/// Render, falling back to the original text on a Liquid error.
fn render_graceful(text: &str, parser: &Parser, object: &Object) -> String {
    render_with(parser, text, object).unwrap_or_else(|_| text.to_string())
}

fn copy_or_render(
    src: &Path,
    dest: &Path,
    parser: &Parser,
    object: &Object,
    stats: &mut GenerationStats,
) -> Result<()> {
    let bytes = std::fs::read(src)?;
    match String::from_utf8(bytes) {
        Ok(text) => {
            let rendered = render_graceful(&text, parser, object);
            std::fs::write(dest, rendered.as_bytes())
                .with_context(|| format!("writing {}", dest.display()))?;
            stats.files_rendered += 1;
        }
        Err(e) => {
            // Binary file: copy bytes verbatim.
            std::fs::write(dest, e.into_bytes())
                .with_context(|| format!("copying {}", dest.display()))?;
            stats.files_copied += 1;
        }
    }
    Ok(())
}

fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p).with_context(|| format!("invalid glob `{p}`"))?);
    }
    Ok(b.build()?)
}

/// Patterns from a template-root `.genignore` file: one glob per line, `#`
/// comments and blank lines skipped. Returns an empty vec when the file is
/// absent. These are *globs* (merged into the `ignore` filter), NOT gitignore —
/// there is no `!` negation or `/`-anchoring semantics, matching the
/// config-level `ignore` globs.
fn load_genignore(template_dir: &Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(template_dir.join(IGNORE_FILE_NAME)) else {
        return Vec::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

fn passes_filters(rel: &str, include: &GlobSet, exclude: &GlobSet, ignore: &GlobSet) -> bool {
    let included = include.is_empty() || include.is_match(rel);
    included && !exclude.is_match(rel) && !ignore.is_match(rel)
}

/// True if a directory itself should be pruned (never descend into it).
fn is_excluded_dir(path: &Path, root: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    never_copy(&rel.to_string_lossy().replace('\\', "/"))
}

fn never_copy(rel: &str) -> bool {
    NEVER_COPY.iter().any(|n| rel == *n || rel.starts_with(&format!("{n}/")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(root: &Path, rel: &str, contents: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, contents).unwrap();
    }

    fn vars(pairs: &[(&str, &str)]) -> Variables {
        let mut v = Variables::default();
        for (k, val) in pairs {
            v.0.insert((*k).into(), (*val).into());
        }
        v
    }

    #[test]
    fn render_template_substitutes() {
        let v = vars(&[("name", "world")]);
        assert_eq!(render_template("hello {{ name }}!", &v).unwrap(), "hello world!");
        // no braces → unchanged
        assert_eq!(render_template("plain text", &v).unwrap(), "plain text");
    }

    #[test]
    fn generate_renders_contents_and_filenames() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        write(src.path(), "README.md", "# {{ name }}\n");
        write(src.path(), "{{ name }}.rs", "pub fn hi() {}\n");
        write(src.path(), "src/util.rs", "// {{ name }}\n");
        // config file must be skipped
        write(src.path(), crate::CONFIG_FILE_NAME, "template { }");
        // .git must be skipped
        write(src.path(), ".git/config", "x");

        let stats = expand(
            src.path(),
            dst.path(),
            &vars(&[("name", "app")]),
            &GenerationOptions::default(),
        )
        .unwrap();

        assert_eq!(fs::read_to_string(dst.path().join("README.md")).unwrap(), "# app\n");
        assert_eq!(fs::read_to_string(dst.path().join("app.rs")).unwrap(), "pub fn hi() {}\n");
        assert_eq!(fs::read_to_string(dst.path().join("src/util.rs")).unwrap(), "// app\n");
        assert!(!dst.path().join(crate::CONFIG_FILE_NAME).exists());
        assert!(!dst.path().join(".git").exists());

        assert_eq!(
            stats,
            GenerationStats {
                dirs_created: 1, // src/
                files_rendered: 3,
                files_copied: 0,
            }
        );
    }

    #[test]
    fn liquid_suffix_is_stripped_and_rendered() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        write(src.path(), "README.md.liquid", "hi {{ name }}");

        expand(
            src.path(),
            dst.path(),
            &vars(&[("name", "x")]),
            &GenerationOptions::default(),
        )
        .unwrap();

        assert!(dst.path().join("README.md").exists());
        assert!(!dst.path().join("README.md.liquid").exists());
        assert_eq!(fs::read_to_string(dst.path().join("README.md")).unwrap(), "hi x");
    }

    #[test]
    fn include_exclude_ignore_filters() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        write(src.path(), "keep.txt", "");
        write(src.path(), "drop.txt", "");
        write(src.path(), "secret.key", "");
        write(src.path(), "nested/keep.txt", "");
        write(src.path(), "nested/drop.txt", "");

        let opts = GenerationOptions {
            exclude: vec!["**/drop.txt".into()],
            ignore: vec!["*.key".into()],
            include: vec![],
        };
        expand(src.path(), dst.path(), &Variables::default(), &opts).unwrap();

        assert!(dst.path().join("keep.txt").exists());
        assert!(dst.path().join("nested/keep.txt").exists());
        assert!(!dst.path().join("drop.txt").exists());
        assert!(!dst.path().join("nested/drop.txt").exists());
        assert!(!dst.path().join("secret.key").exists());
    }

    #[test]
    fn include_whitelist_only_copies_matches() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        write(src.path(), "a.keep", "");
        write(src.path(), "b.skip", "");

        let opts = GenerationOptions {
            include: vec!["*.keep".into()],
            ..Default::default()
        };
        expand(src.path(), dst.path(), &Variables::default(), &opts).unwrap();

        assert!(dst.path().join("a.keep").exists());
        assert!(!dst.path().join("b.skip").exists());
    }

    #[test]
    fn binary_file_copied_verbatim() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let bytes = [0xffu8, 0xfe, 0x00, 0x01, 0xc0]; // invalid UTF-8
        fs::write(src.path().join("blob.bin"), bytes).unwrap();

        let stats = expand(
            src.path(),
            dst.path(),
            &Variables::default(),
            &GenerationOptions::default(),
        )
        .unwrap();

        assert_eq!(fs::read(src.path().join("blob.bin")).unwrap(), bytes);
        assert_eq!(fs::read(dst.path().join("blob.bin")).unwrap(), bytes);
        assert_eq!(stats.files_copied, 1);
        assert_eq!(stats.files_rendered, 0);
    }

    #[test]
    fn case_filters_apply_in_contents_and_filenames() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        write(src.path(), "lib.rs", "// {{ name | snake_case }}\n");
        write(src.path(), "{{ name | kebab_case }}.txt", "pascal={{ name | pascal_case }}");

        expand(
            src.path(),
            dst.path(),
            &vars(&[("name", "My Cool Project")]),
            &GenerationOptions::default(),
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(dst.path().join("lib.rs")).unwrap(),
            "// my_cool_project\n"
        );
        let out = dst.path().join("my-cool-project.txt");
        assert!(out.exists(), "kebab_case filter should render the filename");
        assert_eq!(fs::read_to_string(&out).unwrap(), "pascal=MyCoolProject");
    }

    #[test]
    fn genignore_excludes_patterns_and_itself() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        write(src.path(), "keep.txt", "kept");
        write(src.path(), "secret.key", "shh");
        write(src.path(), "debug.log", "log");
        write(src.path(), "nested/keep.txt", "kept");
        write(src.path(), "nested/debug.log", "log");
        // The .genignore file itself (glob patterns, # comments allowed).
        write(src.path(), crate::generate::IGNORE_FILE_NAME, "# secrets\n*.key\n*.log\n");

        expand(
            src.path(),
            dst.path(),
            &Variables::default(),
            &GenerationOptions::default(),
        )
        .unwrap();

        assert!(dst.path().join("keep.txt").exists());
        assert!(dst.path().join("nested/keep.txt").exists());
        assert!(!dst.path().join("secret.key").exists(), "*.key ignored");
        assert!(!dst.path().join("debug.log").exists(), "*.log ignored");
        assert!(
            !dst.path().join("nested/debug.log").exists(),
            "*.log ignored in subdirs"
        );
        assert!(
            !dst.path().join(crate::generate::IGNORE_FILE_NAME).exists(),
            ".genignore must not be copied out"
        );
    }
}
