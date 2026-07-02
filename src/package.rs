//! `package` subcommand: bundle a template directory into a distributable
//! archive (`zip` / `tar.gz` / `tar.zst`).
//!
//! Thin orchestration on top of [`crate::archive::pack`]: resolves the format
//! and output path from the CLI args, loads the template-root `.genignore`
//! (so secrets can be kept out of a published package), and reports a summary.
//! Packaging copies files **raw** (no Liquid rendering) — the archive is a
//! faithful, regenerable copy of the template, kept verbatim minus `.git` and
//! the `.genignore`-listed paths.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::archive::{self, Format};
use crate::cli::PackageArgs;

/// Entry point for `openeis-generate package`.
pub fn run(args: &PackageArgs) -> Result<()> {
    let src_dir = args
        .path
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));

    if !src_dir.is_dir() {
        bail!(
            "template path is not a directory: {}",
            src_dir.display()
        );
    }

    // Kind-aware manifest check: a template dir has `template.kdl`, a skill
    // bundle has `skills.kdl`. Only warn when NEITHER is present.
    let has_template = src_dir.join(crate::CONFIG_FILE_NAME).exists();
    let has_skills = src_dir.join("skills.kdl").exists();
    if !has_template && !has_skills {
        eprintln!(
            "warning: no template.kdl or skills.kdl found in {} — packaging anyway",
            src_dir.display()
        );
    }

    let fmt = resolve_format(args, &src_dir)?;
    validate_level(args.level, fmt)?;

    let dest = resolve_output(args, &src_dir, fmt)?;
    if dest.exists() && !args.force {
        bail!(
            "output {} already exists; pass --force to overwrite",
            dest.display()
        );
    }

    let ignore = build_ignore_set(&load_genignore(&src_dir))?;
    let stats = archive::pack(&src_dir, &dest, fmt, args.level, &ignore)?;

    println!("packaged {} → {}", src_dir.display(), dest.display());
    println!("  format: {}", fmt.primary_extension());
    println!("  files : {}", stats.files);
    println!("  dirs  : {}", stats.dirs);
    println!("  bytes : {}", stats.bytes);
    Ok(())
}

/// `--format` wins; else infer from `--output`'s extension; else default to
/// `.tar.zst`.
fn resolve_format(args: &PackageArgs, _src_dir: &Path) -> Result<Format> {
    if let Some(fmt) = args.format {
        return Ok(fmt);
    }
    if let Some(out) = &args.output {
        return archive::detect(&out.to_string_lossy()).with_context(|| {
            format!(
                "could not infer archive format from output name `{}`; pass --format \
                 (zip / tar-gz / tar-zst)",
                out.display()
            )
        });
    }
    Ok(Format::TarZst)
}

/// `<basename>.<ext>` when `--output` is omitted. The basename comes from the
/// canonicalized source dir (so `.` resolves to the current directory's name).
fn resolve_output(args: &PackageArgs, src_dir: &Path, fmt: Format) -> Result<PathBuf> {
    if let Some(out) = &args.output {
        return Ok(out.clone());
    }
    let src_abs = src_dir.canonicalize().unwrap_or_else(|_| src_dir.to_path_buf());
    let base = src_abs
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("template");
    Ok(PathBuf::from(format!("{base}.{}", fmt.primary_extension())))
}

/// `.genignore` from the template root: one glob per line, `#`/blank lines
/// skipped. Shape mirrors [`crate::generate`]'s loader.
fn load_genignore(dir: &Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(dir.join(crate::generate::IGNORE_FILE_NAME)) else {
        return Vec::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

fn build_ignore_set(patterns: &[String]) -> Result<globset::GlobSet> {
    let mut b = globset::GlobSetBuilder::new();
    for p in patterns {
        b.add(globset::Glob::new(p).with_context(|| {
            format!("invalid `{}` glob `{p}`", crate::generate::IGNORE_FILE_NAME)
        })?);
    }
    Ok(b.build()?)
}

/// Reject out-of-range `--level` values for the formats that honor it.
/// `zip` doesn't thread the level through (it uses default deflate), so it's
/// not validated here.
fn validate_level(level: Option<i32>, fmt: Format) -> Result<()> {
    let Some(lvl) = level else {
        return Ok(());
    };
    let (lo, hi) = match fmt {
        Format::TarGz => (0, 9),
        Format::TarZst => (1, 22),
        Format::Zip => return Ok(()), // zip: level ignored, no validation
    };
    if !(lo..=hi).contains(&lvl) {
        bail!("--level {lvl} is out of range for {fmt:?} (valid {lo}–{hi})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;
    use std::fs;
    use tempfile::TempDir;

    fn template_tree(root: &Path) {
        fs::write(root.join("template.kdl"), "template { }\n").unwrap();
        fs::write(root.join("README.md.liquid"), "# {{ name }}\n").unwrap();
        fs::write(root.join(crate::generate::IGNORE_FILE_NAME), "secrets.env\n").unwrap();
        fs::write(root.join("secrets.env"), "TOKEN=shh\n").unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "[core]\n").unwrap();
    }

    #[test]
    fn package_tarzst_by_default_and_honors_genignore() {
        let src = TempDir::new().unwrap();
        template_tree(src.path());
        let out_dir = TempDir::new().unwrap();
        let args = PackageArgs {
            path: Some(src.path().to_path_buf()),
            output: Some(out_dir.path().join("pkg.tar.zst")),
            format: None,
            level: None,
            force: false,
        };
        run(&args).unwrap();

        // Round-trip: extract and verify the .genignore secret + .git are gone.
        let extracted = TempDir::new().unwrap();
        crate::archive::extract(
            &out_dir.path().join("pkg.tar.zst"),
            extracted.path(),
            Format::TarZst,
        )
        .unwrap();
        assert!(extracted.path().join("template.kdl").exists());
        assert!(extracted.path().join("README.md.liquid").exists());
        assert!(!extracted.path().join("secrets.env").exists());
        assert!(!extracted.path().join(".git").exists());
    }

    /// `--output` omitted → derive `<dir-basename>.<ext>` from the format.
    /// Tests the pure resolver directly (no global cwd mutation).
    #[test]
    fn resolve_output_derives_name_from_format() {
        let tmp = TempDir::new().unwrap();
        let fmt = Format::TarGz;
        let args = PackageArgs {
            path: Some(tmp.path().to_path_buf()),
            output: None,
            format: Some(fmt),
            level: None,
            force: false,
        };
        let out = resolve_output(&args, tmp.path(), fmt).unwrap();
        let base = tmp.path().file_name().unwrap().to_str().unwrap();
        assert_eq!(out, PathBuf::from(format!("{base}.tar.gz")));

        // Default format (no --output, no --format) is .tar.zst.
        let args = PackageArgs {
            path: Some(tmp.path().to_path_buf()),
            output: None,
            format: None,
            level: None,
            force: false,
        };
        let fmt = resolve_format(&args, tmp.path()).unwrap();
        assert_eq!(fmt, Format::TarZst);
    }

    #[test]
    fn package_refuses_overwrite_without_force() {
        let src = TempDir::new().unwrap();
        template_tree(src.path());
        let out_dir = TempDir::new().unwrap();
        let out = out_dir.path().join("pkg.tar.zst");
        fs::write(&out, b"preexisting").unwrap();

        let args = PackageArgs {
            path: Some(src.path().to_path_buf()),
            output: Some(out.clone()),
            format: None,
            level: None,
            force: false,
        };
        let err = run(&args).unwrap_err();
        assert!(format!("{err}").contains("already exists"));
        // Original file untouched.
        assert_eq!(fs::read(&out).unwrap(), b"preexisting");
    }

    #[test]
    fn package_rejects_bad_level_for_zstd() {
        let src = TempDir::new().unwrap();
        template_tree(src.path());
        let args = PackageArgs {
            path: Some(src.path().to_path_buf()),
            output: Some(src.path().join("ignored.tar.zst")),
            format: Some(Format::TarZst),
            level: Some(99),
            force: true,
        };
        let err = run(&args).unwrap_err();
        assert!(format!("{err}").contains("out of range"));
    }

    /// The subcommand parses into the right variant and forwards the args.
    #[test]
    fn cli_parses_package_subcommand() {
        let cli = Cli::try_parse_from([
            "openeis-generate",
            "package",
            "./tpl",
            "-o",
            "out.tar.zst",
            "--level",
            "3",
        ])
        .expect("parse");
        let Command::Package(a) = cli.command.expect("package subcommand");
        assert_eq!(a.path.as_deref(), Some(std::path::Path::new("./tpl")));
        assert_eq!(a.output.as_deref(), Some(std::path::Path::new("out.tar.zst")));
        assert_eq!(a.level, Some(3));
    }

    /// `openeis-generate --git x` still parses as the flat (generate) path —
    /// adding the subcommand is non-breaking.
    #[test]
    fn flat_generate_still_works_without_subcommand() {
        let cli =
            Cli::try_parse_from(["openeis-generate", "--git", "x", "--name", "n"]).expect("parse");
        assert!(cli.command.is_none(), "no subcommand for the generate path");
        assert_eq!(cli.template.git.as_deref(), Some("x"));
    }
}
