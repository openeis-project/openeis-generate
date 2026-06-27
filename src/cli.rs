//! Command-line interface (clap).
//!
//! Mirrors a focused subset of cargo-generate's `args.rs`. Template expansion
//! isn't implemented yet, so `run()` resolves the inputs against the existing
//! config layer and prints a generation plan (a dry run) rather than writing
//! files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use indexmap::IndexMap;

use crate::archive::Format;
use crate::vcs::Vcs;

/// Help-heading groups, to keep `--help` organized like cargo-generate's.
mod heading {
    pub const TEMPLATE_SELECTION: &str = "Template Selection";
    pub const GIT_PARAMETERS: &str = "Git Parameters";
    pub const OUTPUT_PARAMETERS: &str = "Output Parameters";
}

#[derive(Parser, Debug)]
#[command(
    name = "openeis-generate",
    bin_name = "openeis-generate",
    version,
    about = "Generate a project from a KDL-configured template.",
    arg_required_else_help(true),
    propagate_version = true,
)]
pub struct Cli {
    #[command(flatten)]
    pub template: TemplateSource,

    /// Subcommand (`package`). When `None`, the flat generate flags below run.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// List favorite templates defined in the app config, then exit.
    #[arg(long, group = "ModeSelector")]
    pub list_favorites: bool,

    /// Project name / output directory. If not kebab-case it is converted unless --force.
    #[arg(long, short, help_heading = heading::OUTPUT_PARAMETERS)]
    pub name: Option<String>,

    /// Don't convert the project name to kebab-case.
    #[arg(long, short, help_heading = heading::OUTPUT_PARAMETERS)]
    pub force: bool,

    /// App config file (default: ~/.config/openeis/openeis.kdl).
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// VCS to initialize after generation (Git | None).
    #[arg(long, help_heading = heading::OUTPUT_PARAMETERS)]
    pub vcs: Option<Vcs>,

    /// Define a template variable as KEY=VALUE. Repeatable.
    #[arg(long, short = 'D', value_name = "KEY=VALUE", help_heading = heading::OUTPUT_PARAMETERS)]
    pub define: Vec<String>,

    /// Load template variables from a KDL file (`values { key "value" }`).
    /// CLI --define entries take precedence over values from this file.
    #[arg(long, value_name = "FILE", help_heading = heading::OUTPUT_PARAMETERS)]
    pub values_file: Option<PathBuf>,

    /// Generate in place into the current directory (no subfolder, no VCS).
    #[arg(long, help_heading = heading::OUTPUT_PARAMETERS)]
    pub init: bool,

    /// Destination directory for the generated project.
    #[arg(long, value_name = "PATH", help_heading = heading::OUTPUT_PARAMETERS)]
    pub destination: Option<PathBuf>,

    /// Allow overwriting existing files in the destination.
    #[arg(long, help_heading = heading::OUTPUT_PARAMETERS)]
    pub overwrite: bool,

    /// Force a fresh `git init` even if the destination is already a git repo.
    #[arg(long, help_heading = heading::OUTPUT_PARAMETERS)]
    pub force_git_init: bool,

    /// Allow template hooks to run arbitrary system commands (dangerous: review
    /// the template first). Registers the rhai `system` module.
    #[arg(short = 's', long, help_heading = heading::OUTPUT_PARAMETERS)]
    pub allow_commands: bool,

    /// More verbose output.
    #[arg(long, short, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Suppress warnings/errors.
    #[arg(long, short, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Keep going when template errors are encountered.
    #[arg(long)]
    pub continue_on_error: bool,

    /// Don't prompt: every placeholder must resolve from --define or its default.
    #[arg(long, requires = "name")]
    pub silent: bool,

    /// Resolve and print the plan without writing any files.
    #[arg(long)]
    pub dry_run: bool,
}

/// Where and how to fetch the template.
#[derive(Args, Debug, Default, Clone)]
pub struct TemplateSource {
    /// Favorite name (from app config), or subfolder when --git/--path is given.
    #[arg(help_heading = heading::TEMPLATE_SELECTION)]
    pub auto_path: Option<String>,

    /// Subfolder within the template repository to use as the template.
    #[arg(long, help_heading = heading::TEMPLATE_SELECTION)]
    pub subfolder: Option<String>,

    /// Git repository to clone the template from (URL, path, or owner/repo).
    #[arg(short, long, group = "SpecificPath", help_heading = heading::TEMPLATE_SELECTION)]
    pub git: Option<String>,

    /// Local path to copy the template from.
    #[arg(short, long, group = "SpecificPath", help_heading = heading::TEMPLATE_SELECTION)]
    pub path: Option<String>,

    /// Archive to extract the template from: a local `.zip`/`.tar.gz` file or an HTTP(S) URL.
    #[arg(long, group = "SpecificPath", help_heading = heading::TEMPLATE_SELECTION)]
    pub archive: Option<String>,

    /// Favorite template (from app config) to generate.
    #[arg(long, group = "SpecificPath", help_heading = heading::TEMPLATE_SELECTION)]
    pub favorite: Option<String>,

    /// Git branch to use.
    #[arg(short, long, conflicts_with_all = ["tag", "revision"], help_heading = heading::GIT_PARAMETERS)]
    pub branch: Option<String>,

    /// Git tag to use.
    #[arg(short, long, conflicts_with_all = ["branch", "revision"], help_heading = heading::GIT_PARAMETERS)]
    pub tag: Option<String>,

    /// Git revision (commit hash) to use.
    #[arg(short, long, conflicts_with_all = ["branch", "tag"], help_heading = heading::GIT_PARAMETERS)]
    pub revision: Option<String>,
}

impl TemplateSource {
    /// Classify which kind of source was specified.
    pub fn kind(&self) -> Option<TemplateSourceKind> {
        if self.git.is_some() {
            Some(TemplateSourceKind::Git)
        } else if self.path.is_some() {
            Some(TemplateSourceKind::Path)
        } else if self.archive.is_some() {
            Some(TemplateSourceKind::Archive)
        } else if self.favorite.is_some() || self.auto_path.is_some() {
            Some(TemplateSourceKind::Favorite)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateSourceKind {
    Git,
    Path,
    Archive,
    Favorite,
}

/// Top-level subcommands. The default (no subcommand) is the `generate` flow
/// driven by the flat flags on [`Cli`]; `package` bundles a template into a
/// distributable archive.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Package a template directory into a distributable archive
    /// (zip / tar.gz / tar.zst) — for sharing and as a `publish` precursor.
    Package(PackageArgs),
}

/// Arguments for `openeis-generate package`.
#[derive(Args, Debug, Clone)]
pub struct PackageArgs {
    /// Template directory to package (must contain `template.kdl`). Defaults to
    /// the current directory.
    pub path: Option<PathBuf>,

    /// Output archive. The format is auto-detected from the extension
    /// (`.zip` / `.tar.gz` / `.tgz` / `.tar.zst` / `.tzst`); pass `--format` to
    /// force it. Defaults to `<dir-name>.tar.zst`.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Force a format, ignoring the `--output` extension (zip / tar-gz / tar-zst).
    #[arg(long, value_enum)]
    pub format: Option<Format>,

    /// Compression level. zstd 1–22 (default 3), gzip 0–9 (default 6); ignored
    /// for zip.
    #[arg(long)]
    pub level: Option<i32>,

    /// Overwrite an existing output file.
    #[arg(short = 'f', long)]
    pub force: bool,
}

/// Entry point used by `main`. Resolves the template source (cloning for git
/// sources), collects variables, and expands the template into the destination.
pub fn run(cli: &Cli) -> Result<()> {
    if let Some(Command::Package(args)) = &cli.command {
        return crate::package::run(args);
    }

    let app_cfg = load_app_config(&cli.config)?;

    if cli.list_favorites {
        return list_favorites(&app_cfg, &cli.config);
    }

    let handle = prepare_source(cli, &app_cfg)?;

    // --define merged with --values-file (CLI --define takes precedence).
    let defines = merged_defines(cli)?;

    if cli.dry_run {
        println!("source      : {} ({:?})", handle.source_desc, handle.kind);
        println!("template dir: {}", handle.dir.display());
        print_template_config(&handle.config);
        summarize_variables(&handle.config, &defines)?;
        return Ok(());
    }

    // Resolve the project name (prompt if --name was omitted), then the
    // destination from it. Done after the dry-run branch so --dry-run stays
    // non-interactive and name-optional.
    let name = resolve_project_name(cli)?;
    // Resolve destination early so hooks (env::destination) can see it.
    let dest = resolve_dest(cli, &name)?;

    // init hooks: run in the template dir, before variable collection.
    let mut init_vars = crate::Variables::default();
    run_hooks(
        crate::HookPhase::Init,
        &handle.config.hooks_for(crate::HookPhase::Init),
        &handle.dir,
        &handle.dir,
        &dest,
        &mut init_vars,
        cli.allow_commands,
    )?;

    // Collect base + conditional placeholders (built-ins seeded from the name),
    // and merge matching conditionals' include/exclude/ignore into `opts`.
    let (mut vars, opts) = crate::conditional::collect(
        &handle.config,
        &defines,
        cli.silent,
        Some(name.as_str()),
        cli.init,
    )?;

    // pre hooks: variables are known; run in the template dir, before expand.
    // `variable::set` here overrides/adds variables that feed the expansion.
    run_hooks(
        crate::HookPhase::Pre,
        &handle.config.hooks_for(crate::HookPhase::Pre),
        &handle.dir,
        &handle.dir,
        &dest,
        &mut vars,
        cli.allow_commands,
    )?;

    let stats = crate::generate::expand(&handle.dir, &dest, &vars, &opts)?;

    println!("generated {} → {}", handle.source_desc, dest.display());
    println!("  dirs created : {}", stats.dirs_created);
    println!("  rendered     : {}", stats.files_rendered);
    println!("  copied       : {}", stats.files_copied);

    // post hooks: run in the destination dir, after expand (before VCS init so
    // hook-produced changes land in the initial commit).
    run_hooks(
        crate::HookPhase::Post,
        &handle.config.hooks_for(crate::HookPhase::Post),
        &dest,
        &handle.dir,
        &dest,
        &mut vars,
        cli.allow_commands,
    )?;

    // VCS init (skip for --init, which generates in place with no VCS).
    if !cli.init {
        init_vcs(cli, &handle, &dest)?;
    }
    Ok(())
}

/// Run `scripts` as rhai hooks. `working_dir` is the base for `file::` ops
/// (template dir for init/pre, destination for post); `script_dir` is always the
/// template dir where `.rhai` files live; `destination` is the output dir
/// (`env::destination()`). `vars` is borrowed mutably so pre-hook `variable::set`
/// calls flow into the expansion. No-op if there are no scripts.
fn run_hooks(
    phase: crate::HookPhase,
    scripts: &[String],
    working_dir: &Path,
    script_dir: &Path,
    destination: &Path,
    vars: &mut crate::Variables,
    allow_commands: bool,
) -> Result<()> {
    if scripts.is_empty() {
        return Ok(());
    }
    println!(
        "running {phase:?} hooks ({}) in {}…",
        scripts.len(),
        working_dir.display()
    );
    let mut ctx = crate::hooks::HookContext {
        variables: vars,
        working_dir,
        script_dir,
        destination,
        allow_commands,
    };
    crate::hooks::run(scripts, &mut ctx)
}

/// Resolve the effective VCS (CLI flag overrides the template default) and
/// initialize it in the generated project. `None` is a no-op.
fn init_vcs(cli: &Cli, handle: &TemplateHandle, dest: &Path) -> Result<()> {
    let vcs = cli.vcs.or(handle.config.template.vcs).unwrap_or_default();
    if vcs.is_none() {
        return Ok(());
    }
    let branch = cli.template.branch.as_deref();
    print!("initializing {:?} in {}… ", vcs, dest.display());
    match vcs.initialize(dest, branch, cli.force_git_init) {
        Ok(()) => {
            println!("done");
            Ok(())
        }
        Err(e) => {
            println!("failed");
            // Files are already written; a git-init failure shouldn't discard them.
            eprintln!("warning: VCS init failed: {e}");
            Ok(())
        }
    }
}

/// A resolved template on disk, ready to expand.
struct TemplateHandle {
    kind: TemplateSourceKind,
    dir: PathBuf,
    config: crate::Config,
    source_desc: String,
    /// Owns the clone tempdir; dropped at the end of `run`. None for local paths.
    _temp: Option<tempfile::TempDir>,
}

/// Resolve the template to a directory on disk: a local --path as-is, or a git
/// clone (for --git / a git-backed --favorite) into a temp dir.
fn prepare_source(cli: &Cli, app_cfg: &crate::AppConfig) -> Result<TemplateHandle> {
    let src = &cli.template;
    let kind = src.kind().context(
        "no template source: pass --git <url>, --path <dir>, --archive <file|url>, \
         --favorite <name>, or a positional favorite name",
    )?;

    match kind {
        TemplateSourceKind::Path => {
            let p = src.path.clone().unwrap();
            let sub = src.subfolder.clone().or_else(|| src.auto_path.clone());
            let dir = resolve_subfolder(PathBuf::from(&p), sub);
            let config = load_config_at(&dir)?;
            Ok(TemplateHandle { kind, dir, config, source_desc: p, _temp: None })
        }
        TemplateSourceKind::Git => {
            let url = src.git.clone().unwrap();
            let temp = tempfile::TempDir::new()?;
            crate::git::clone(&url, temp.path(), make_ref(src, None).as_ref())
                .with_context(|| format!("cloning {url}"))?;
            let sub = src.subfolder.clone().or_else(|| src.auto_path.clone());
            let dir = resolve_subfolder(temp.path().to_path_buf(), sub);
            let config = load_config_at(&dir)?;
            Ok(TemplateHandle { kind, dir, config, source_desc: url, _temp: Some(temp) })
        }
        TemplateSourceKind::Archive => {
            let src_url = src.archive.clone().unwrap();
            let fmt = crate::archive::detect(&src_url).with_context(|| {
                format!("unsupported archive format for `{src_url}` (use .zip / .tar.gz / .tgz)")
            })?;
            // One temp dir holds the (downloaded) archive and the extracted template.
            let work = tempfile::TempDir::new()?;
            let archive_file = if crate::archive::is_url(&src_url) {
                let f = work.path().join("archive");
                crate::archive::download(&src_url, &f)?;
                f
            } else {
                PathBuf::from(&src_url)
            };
            let extracted = work.path().join("tmpl");
            crate::archive::extract(&archive_file, &extracted, fmt)?;
            let sub = src.subfolder.clone().or_else(|| src.auto_path.clone());
            let dir = resolve_subfolder(extracted, sub);
            let config = load_config_at(&dir)?;
            Ok(TemplateHandle { kind, dir, config, source_desc: src_url, _temp: Some(work) })
        }
        TemplateSourceKind::Favorite => {
            let name = src.favorite.clone().or_else(|| src.auto_path.clone()).unwrap();
            let fav = app_cfg
                .get_favorite_cfg(&name)
                .with_context(|| format!("favorite '{name}' is not defined in the app config"))?;
            if let Some(url) = &fav.git {
                let temp = tempfile::TempDir::new()?;
                crate::git::clone(url, temp.path(), make_ref(src, Some(fav)).as_ref())
                    .with_context(|| format!("cloning {url} (favorite {name})"))?;
                let sub = src.subfolder.clone().or_else(|| fav.subfolder.clone());
                let dir = resolve_subfolder(temp.path().to_path_buf(), sub);
                let config = load_config_at(&dir)?;
                Ok(TemplateHandle {
                    kind,
                    dir,
                    config,
                    source_desc: format!("favorite {name}: {url}"),
                    _temp: Some(temp),
                })
            } else if let Some(p) = &fav.path {
                let sub = src.subfolder.clone().or_else(|| fav.subfolder.clone());
                let dir = resolve_subfolder(p.clone(), sub);
                let config = load_config_at(&dir)?;
                Ok(TemplateHandle {
                    kind,
                    dir,
                    config,
                    source_desc: format!("favorite {name}: {}", p.display()),
                    _temp: None,
                })
            } else {
                anyhow::bail!("favorite '{name}' has neither `git` nor `path`");
            }
        }
    }
}

/// Choose a git ref: CLI flags win, then the favorite's defaults.
fn make_ref(src: &TemplateSource, fav: Option<&crate::FavoriteConfig>) -> Option<crate::git::GitRef> {
    if let Some(b) = &src.branch {
        return Some(crate::git::GitRef::Branch(b.clone()));
    }
    if let Some(t) = &src.tag {
        return Some(crate::git::GitRef::Tag(t.clone()));
    }
    if let Some(r) = &src.revision {
        return Some(crate::git::GitRef::Revision(r.clone()));
    }
    if let Some(f) = fav {
        if let Some(b) = &f.branch {
            return Some(crate::git::GitRef::Branch(b.clone()));
        }
        if let Some(t) = &f.tag {
            return Some(crate::git::GitRef::Tag(t.clone()));
        }
        if let Some(r) = &f.revision {
            return Some(crate::git::GitRef::Revision(r.clone()));
        }
    }
    None
}

fn resolve_subfolder(root: PathBuf, sub: Option<String>) -> PathBuf {
    match sub {
        Some(s) => root.join(s),
        None => root,
    }
}

fn load_config_at(dir: &Path) -> Result<crate::Config> {
    let cfg_path = dir.join(crate::CONFIG_FILE_NAME);
    if cfg_path.exists() {
        crate::Config::from_path(&Some(cfg_path))
    } else {
        Ok(crate::Config::default())
    }
}

fn resolve_dest(cli: &Cli, name: &str) -> Result<PathBuf> {
    if cli.init {
        return Ok(PathBuf::from("."));
    }
    if let Some(d) = &cli.destination {
        return Ok(d.clone());
    }
    Ok(PathBuf::from(name))
}

/// Resolve the project name: an explicit `--name` wins; otherwise prompt for it
/// interactively. `--silent` carries `requires = "name"` (see [`Cli::silent`]),
/// so this only reaches the prompt in interactive runs — it never blocks silent
/// mode. Returns the trimmed name (also seeds the `project-name`/`crate_name`
/// built-ins).
fn resolve_project_name(cli: &Cli) -> Result<String> {
    if let Some(n) = &cli.name {
        return Ok(n.clone());
    }
    use dialoguer::Input;
    let name: String = Input::new()
        .with_prompt("Project name")
        .validate_with(|s: &String| {
            (!s.trim().is_empty()).then_some(()).ok_or("name cannot be empty")
        })
        .interact_text()?;
    Ok(name.trim().to_string())
}

/// Merge `--define` entries with a `--values-file` into one ordered map.
/// CLI `--define` entries take precedence over values from the file.
fn merged_defines(cli: &Cli) -> Result<IndexMap<String, String>> {
    let mut map = crate::parse_defines(&cli.define)?;
    if let Some(path) = &cli.values_file {
        let file_vals = crate::variables::load_values_file(path)?;
        for (k, v) in file_vals {
            map.entry(k).or_insert(v);
        }
    }
    Ok(map)
}

/// Print how each placeholder would be resolved. Non-blocking: it never prompts
/// — it only classifies each as defined / default / would-prompt (or reports a
/// validation error against the supplied `--define` / `--values-file` values).
fn summarize_variables(cfg: &crate::Config, defines: &IndexMap<String, String>) -> Result<()> {
    let Some(placeholders) = cfg.placeholders.as_ref() else {
        return Ok(());
    };
    if placeholders.is_empty() {
        return Ok(());
    }
    println!("variables:");
    for (name, p) in placeholders {
        let provided = defines.get(name).map(String::as_str);
        match crate::variables::resolve(name, p, provided, false) {
            Ok(crate::variables::Resolved::Value(v)) => println!("  {name}: defined = {v}"),
            Ok(crate::variables::Resolved::PromptString { default, .. }) => {
                println!(
                    "  {name}: would prompt (string, default {})",
                    default.as_deref().unwrap_or("<none>")
                );
            }
            Ok(crate::variables::Resolved::PromptBool { default }) => {
                println!(
                    "  {name}: would prompt (bool, default {})",
                    default.unwrap_or(false)
                );
            }
            Ok(crate::variables::Resolved::PromptArray { default, .. }) => {
                println!(
                    "  {name}: would prompt (array, default [{}])",
                    default.join(", ")
                );
            }
            Err(e) => println!("  {name}: INVALID — {e}"),
        }
    }
    Ok(())
}

/// Load the app config from the given path, or the default location.
fn load_app_config(config: &Option<PathBuf>) -> Result<crate::AppConfig> {
    let path = crate::app_config_path(config)?;
    // Missing default config is fine — just means no favorites.
    if !path.exists() {
        return Ok(crate::AppConfig::default());
    }
    crate::AppConfig::try_from(path.as_path())
}

fn list_favorites(app_cfg: &crate::AppConfig, config: &Option<PathBuf>) -> Result<()> {
    let resolved = crate::app_config_path(config)?;
    println!("app config: {}", resolved.display());
    match app_cfg.favorites.as_ref() {
        Some(favorites) if !favorites.is_empty() => {
            for (name, fav) in favorites {
                let where_ = fav
                    .git
                    .clone()
                    .or_else(|| fav.path.as_ref().map(|p| p.display().to_string()));
                println!("- {name}: {}", where_.unwrap_or_else(|| "(no source)".into()));
                if let Some(desc) = &fav.description {
                    println!("    {desc}");
                }
            }
        }
        _ => println!("(no favorites defined)"),
    }
    Ok(())
}

fn print_template_config(cfg: &crate::Config) {
    println!("template config ({}):", crate::CONFIG_FILE_NAME);
    let t = &cfg.template;
    if let Some(v) = &t.generator_version {
        println!("  generator_version: {v}");
    }
    if let Some(inc) = &t.include {
        println!("  include: {inc:?}");
    }
    if !cfg.all_hooks().is_empty() {
        println!("  hooks: {:?}", cfg.all_hooks());
    }
    if let Some(placeholders) = &cfg.placeholders {
        println!("  placeholders:");
        for (name, slot) in placeholders {
            println!("    {name}: {} — {:?}", slot.r#type, slot.prompt);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_source_with_name_and_define() {
        let cli = Cli::try_parse_from([
            "openeis-generate",
            "--git",
            "https://example.com/t.git",
            "--branch",
            "main",
            "--name",
            "my-proj",
            "-D",
            "foo=bar",
        ])
        .expect("parse");
        assert_eq!(cli.template.git.as_deref(), Some("https://example.com/t.git"));
        assert_eq!(cli.template.branch.as_deref(), Some("main"));
        assert_eq!(cli.name.as_deref(), Some("my-proj"));
        assert_eq!(cli.define, vec!["foo=bar".to_string()]);
        assert_eq!(cli.template.kind(), Some(TemplateSourceKind::Git));
    }

    #[test]
    fn parses_path_source_and_auto_path_as_subfolder() {
        let cli = Cli::try_parse_from([
            "openeis-generate",
            "--path",
            "./tpl",
            "sub",
        ])
        .expect("parse");
        assert_eq!(cli.template.path.as_deref(), Some("./tpl"));
        assert_eq!(cli.template.auto_path.as_deref(), Some("sub"));
        assert_eq!(cli.template.kind(), Some(TemplateSourceKind::Path));
    }

    #[test]
    fn branch_tag_revision_are_mutually_exclusive() {
        let r = Cli::try_parse_from(["openeis-generate", "--git", "x", "--branch", "b", "--tag", "t"]);
        assert!(r.is_err(), "branch + tag should conflict");
    }

    #[test]
    fn git_path_favorite_belong_to_one_group() {
        let r = Cli::try_parse_from(["openeis-generate", "--git", "x", "--path", "y"]);
        assert!(r.is_err(), "--git and --path should conflict");
    }

    #[test]
    fn verbose_quiet_conflict() {
        let r = Cli::try_parse_from(["openeis-generate", "--git", "x", "--verbose", "--quiet"]);
        assert!(r.is_err(), "--verbose and --quiet should conflict");
    }

    #[test]
    fn list_favorites_flag_parses() {
        let cli = Cli::try_parse_from(["openeis-generate", "--list-favorites"]).expect("parse");
        assert!(cli.list_favorites);
    }
}
