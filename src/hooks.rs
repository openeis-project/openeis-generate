//! Rhai hooks (init / pre / post).
//!
//! Focused port of cargo-generate's `hooks` module. Each hook entry is either a
//! path to a `.rhai` file (relative to the working dir) or an inline rhai script.
//! Template variables are readable by name; a `file` module (sandboxed to the
//! working dir) is provided, and a `system` module is registered only when
//! `allow_commands` is set (`--allow-commands`).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::Result;
use indexmap::IndexMap;
use rhai::{Dynamic, Engine, EvalAltResult, Module};

use crate::variables::Variables;

type HookResult<T> = std::result::Result<T, Box<EvalAltResult>>;

pub struct HookContext<'a> {
    /// Variables the hooks can read (by name) and, via `variable::set`, override
    /// or add. Pre-hook writes flow into the subsequent expansion.
    pub variables: &'a mut Variables,
    /// Base directory for `file::` operations (the template dir for init/pre,
    /// the destination for post).
    pub working_dir: &'a Path,
    /// Where `.rhai` hook scripts are resolved — always the template dir, so a
    /// post hook's script is found even though the file ops target the dest.
    pub script_dir: &'a Path,
    /// The final output directory (exposed as `env::destination()`).
    pub destination: &'a Path,
    pub allow_commands: bool,
}

/// Execute each hook entry. If `script_dir.join(entry)` is an existing file it
/// is run as a rhai file; otherwise the entry is evaluated as an inline script.
/// Any `variable::set` calls are written back into `ctx.variables`.
pub fn run(scripts: &[String], ctx: &mut HookContext) -> Result<()> {
    if scripts.is_empty() {
        return Ok(());
    }
    // Shared, interior-mutable store so `on_var` (read) and `variable::set`
    // (write) see the same map, and changes propagate back to the caller.
    let shared: Rc<RefCell<IndexMap<String, String>>> =
        Rc::new(RefCell::new(ctx.variables.0.clone()));

    let mut engine = Engine::new();
    register_shared_variables(&mut engine, shared.clone());
    engine.register_static_module("variable", variable_module(shared.clone()).into());
    engine.register_static_module("file", file_module(ctx.working_dir).into());
    engine.register_static_module(
        "env",
        env_module(ctx.working_dir, ctx.destination).into(),
    );
    register_case_functions(&mut engine);
    if ctx.allow_commands {
        engine.register_static_module("system", system_module().into());
    }

    for entry in scripts {
        let path = ctx.script_dir.join(entry);
        let res = if path.is_file() {
            engine.run_file(path)
        } else {
            engine.run(entry.as_str())
        };
        // Box<EvalAltResult> isn't Send+Sync (rhai without the `sync` feature),
        // so we can't use anyhow's `.with_context` — convert via Display.
        res.map_err(|e| anyhow::anyhow!("hook failed: {entry}: {e}"))?;
    }

    // Propagate any `variable::set` changes back to the caller.
    ctx.variables.0 = shared.borrow().clone();
    Ok(())
}

/// Make every template variable readable by its name in rhai (`author`, …),
/// reading from a shared store so `variable::set` writes are visible too.
#[allow(deprecated)] // rhai marks `on_var` "volatile" but it is the documented hook.
fn register_shared_variables(engine: &mut Engine, shared: Rc<RefCell<IndexMap<String, String>>>) {
    engine.on_var(move |name: &str, _index, _context| {
        Ok(shared
            .borrow()
            .get(name)
            .map(|v| Dynamic::from(v.clone())))
    });
}

/// `variable::is_set`, `variable::get`, `variable::set` over the shared store.
/// `set` mutates the map, so pre-hook overrides flow into the expansion.
fn variable_module(shared: Rc<RefCell<IndexMap<String, String>>>) -> Module {
    let mut m = Module::new();

    {
        let s = shared.clone();
        m.set_native_fn("is_set", move |name: &str| -> HookResult<bool> {
            Ok(s.borrow().contains_key(name))
        });
    }
    {
        let s = shared.clone();
        m.set_native_fn("get", move |name: &str| -> HookResult<Dynamic> {
            Ok(s
                .borrow()
                .get(name)
                .map(|v| Dynamic::from(v.clone()))
                .unwrap_or(Dynamic::UNIT))
        });
    }
    {
        let s = shared.clone();
        m.set_native_fn("set", move |name: &str, value: &str| -> HookResult<()> {
            s.borrow_mut()
                .insert(name.to_string(), value.to_string());
            Ok(())
        });
    }
    m
}

/// Evaluate a single boolean rhai expression against the variables (used for
/// `conditional { "<expr>" … }` keys).
///
/// Returns `None` when the expression references a variable that isn't collected
/// yet — this is a *transient* state during the conditional merge loop (a
/// placeholder added by an earlier conditional may not be resolved until the
/// next iteration), so the loop retries rather than failing. A genuine syntax
/// error still returns `Err`.
#[allow(deprecated)]
pub fn eval_condition(expr: &str, vars: &Variables) -> Result<Option<bool>> {
    let mut engine = Engine::new();
    let shared = Rc::new(RefCell::new(vars.0.clone()));
    register_shared_variables(&mut engine, shared);
    match engine.eval_expression::<bool>(expr) {
        Ok(b) => Ok(Some(b)),
        Err(e) => {
            if matches!(*e, EvalAltResult::ErrorVariableNotFound(..)) {
                Ok(None)
            } else {
                Err(anyhow::anyhow!("conditional `{expr}` failed: {e}"))
            }
        }
    }
}

/// `file::` operations, all confined to `base` (path traversal is rejected).
fn file_module(base: &Path) -> Module {
    let mut m = Module::new();

    {
        let b = base.to_path_buf();
        m.set_native_fn("exists", move |path: &str| -> HookResult<bool> {
            Ok(sandbox(&b, path)?.exists())
        });
    }
    {
        let b = base.to_path_buf();
        m.set_native_fn("read", move |path: &str| -> HookResult<String> {
            let p = sandbox(&b, path)?;
            Ok(std::fs::read_to_string(p).map_err(|e| e.to_string())?)
        });
    }
    {
        let b = base.to_path_buf();
        m.set_native_fn("write", move |file: &str, content: &str| -> HookResult<()> {
            let p = sandbox(&b, file)?;
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(p, content).map_err(|e| e.to_string())?;
            Ok(())
        });
    }
    {
        let b = base.to_path_buf();
        m.set_native_fn("delete", move |file: &str| -> HookResult<()> {
            let p = sandbox(&b, file)?;
            if p.is_file() {
                std::fs::remove_file(p).map_err(|e| e.to_string())?;
            } else if p.is_dir() {
                std::fs::remove_dir_all(p).map_err(|e| e.to_string())?;
            }
            Ok(())
        });
    }
    {
        let b = base.to_path_buf();
        m.set_native_fn("rename", move |from: &str, to: &str| -> HookResult<()> {
            let f = sandbox(&b, from)?;
            let t = sandbox(&b, to)?;
            std::fs::rename(f, t).map_err(|e| e.to_string())?;
            Ok(())
        });
    }
    m
}

/// `system::run(cmd)` runs a command via `sh -c` and returns its stdout. Only
/// registered under `--allow-commands`; running arbitrary shell is dangerous,
/// hence the opt-in.
fn system_module() -> Module {
    let mut m = Module::new();
    m.set_native_fn("run", |cmd: &str| -> HookResult<String> {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr)
                .trim()
                .to_string()
                .into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    });
    m
}

/// Exposes the working and destination directories as module constants
/// `env::working_dir` and `env::destination` (no parens — they are values).
fn env_module(working: &Path, destination: &Path) -> Module {
    let mut m = Module::new();
    m.set_var("working_dir", working.display().to_string());
    m.set_var("destination", destination.display().to_string());
    m
}

/// Register global case-conversion helpers (`to_kebab_case`, …) via `heck`.
fn register_case_functions(engine: &mut Engine) {
    use heck::{
        ToKebabCase, ToLowerCamelCase, ToPascalCase, ToShoutyKebabCase, ToShoutySnakeCase,
        ToSnakeCase, ToTitleCase, ToUpperCamelCase,
    };
    engine
        .register_fn("to_kebab_case", |s: &str| s.to_kebab_case())
        .register_fn("to_snake_case", |s: &str| s.to_snake_case())
        .register_fn("to_upper_camel_case", |s: &str| s.to_upper_camel_case())
        .register_fn("to_lower_camel_case", |s: &str| s.to_lower_camel_case())
        .register_fn("to_pascal_case", |s: &str| s.to_pascal_case())
        .register_fn("to_title_case", |s: &str| s.to_title_case())
        .register_fn("to_shouty_snake_case", |s: &str| s.to_shouty_snake_case())
        .register_fn("to_shouty_kebab_case", |s: &str| s.to_shouty_kebab_case());
}

/// Resolve `rel` against `base` and ensure the result stays inside `base`
/// (rejects `..` and absolute escapes). Non-existent targets are allowed when
/// their (existing) parent is within `base`, so `file::write` to a new file works.
fn sandbox(base: &Path, rel: &str) -> HookResult<PathBuf> {
    let joined = base.join(rel);
    let canonical_base = base.canonicalize().map_err(|e| e.to_string())?;
    let canonical = match joined.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            // Target doesn't exist yet (e.g. a file about to be written).
            let Some(parent) = joined.parent() else {
                return Err(format!("invalid path `{rel}`").into());
            };
            let canonical_parent = parent.canonicalize().map_err(|e| e.to_string())?;
            match joined.file_name() {
                Some(name) => canonical_parent.join(name),
                None => return Err(format!("invalid path `{rel}`").into()),
            }
        }
    };
    if !canonical.starts_with(&canonical_base) {
        return Err(format!("path `{rel}` escapes the working directory").into());
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn vars(pairs: &[(&str, &str)]) -> Variables {
        let mut v = Variables::default();
        for (k, val) in pairs {
            v.0.insert((*k).into(), (*val).into());
        }
        v
    }

    fn ctx<'a>(variables: &'a mut Variables, dir: &'a Path) -> HookContext<'a> {
        HookContext {
            variables,
            working_dir: dir,
            script_dir: dir,
            destination: dir,
            allow_commands: false,
        }
    }

    #[test]
    fn inline_hook_reads_variable_and_writes_file() {
        let dir = TempDir::new().unwrap();
        let mut v = vars(&[("author", "Bob")]);
        run(
            &[r#"file::write("note.txt", "hi " + author)"#.to_string()],
            &mut ctx(&mut v, dir.path()),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "hi Bob"
        );
    }

    #[test]
    fn hook_runs_rhai_file_when_path_exists() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("gen.rhai"), r#"file::write("a.txt", "x")"#).unwrap();
        let mut v = Variables::default();
        run(
            &["gen.rhai".to_string()],
            &mut ctx(&mut v, dir.path()),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "x");
    }

    #[test]
    fn empty_scripts_is_noop() {
        let dir = TempDir::new().unwrap();
        let mut v = Variables::default();
        assert!(run(&[], &mut ctx(&mut v, dir.path())).is_ok());
    }

    #[test]
    fn file_module_rejects_path_escape() {
        let dir = TempDir::new().unwrap();
        let mut v = Variables::default();
        let res = run(
            &[r#"file::write("../escape.txt", "x")"#.to_string()],
            &mut ctx(&mut v, dir.path()),
        );
        assert!(res.is_err(), "escape should be rejected");
        assert!(!dir.path().parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn system_module_gated_by_allow_commands() {
        let dir = TempDir::new().unwrap();
        let mut v = Variables::default();

        // Without allow_commands, `system::run` is not registered → error.
        let blocked = run(
            &[r#"system::run("echo hi")"#.to_string()],
            &mut ctx(&mut v, dir.path()),
        );
        assert!(blocked.is_err());

        // With allow_commands, it runs.
        let mut c = ctx(&mut v, dir.path());
        c.allow_commands = true;
        run(
            &[r#"let s = system::run("echo hi"); file::write("o.txt", s)"#.to_string()],
            &mut c,
        )
        .unwrap();
        assert!(fs::read_to_string(dir.path().join("o.txt"))
            .unwrap()
            .contains("hi"));
    }

    #[test]
    fn variable_set_flows_back_to_caller() {
        let dir = TempDir::new().unwrap();
        let mut v = vars(&[("author", "Bob")]);
        run(
            &[
                r#"variable::set("author", "Carol")"#.to_string(),
                r#"variable::set("extra", "added")"#.to_string(),
            ],
            &mut ctx(&mut v, dir.path()),
        )
        .unwrap();
        assert_eq!(v.get("author"), Some("Carol"), "set overrides existing");
        assert_eq!(v.get("extra"), Some("added"), "set adds a new variable");
    }

    #[test]
    fn variable_get_and_is_set() {
        let dir = TempDir::new().unwrap();
        let mut v = vars(&[("author", "Bob")]);
        run(
            &[r#"file::write("o.txt", variable::get("author") + "|" + variable::is_set("missing").to_string())"#.to_string()],
            &mut ctx(&mut v, dir.path()),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("o.txt")).unwrap(),
            "Bob|false"
        );
    }

    #[test]
    fn env_module_exposes_dirs() {
        let dir = TempDir::new().unwrap();
        let mut v = Variables::default();
        run(
            &[r#"file::write("dirs.txt", env::working_dir + "\n" + env::destination)"#.to_string()],
            &mut ctx(&mut v, dir.path()),
        )
        .unwrap();
        let content = fs::read_to_string(dir.path().join("dirs.txt")).unwrap();
        assert_eq!(content.lines().count(), 2);
        assert!(
            content.starts_with(&dir.path().display().to_string()),
            "working_dir should be the dir"
        );
    }

    #[test]
    fn case_conversion_functions() {
        let dir = TempDir::new().unwrap();
        let mut v = Variables::default();
        run(
            &[r#"file::write("c.txt", to_kebab_case("FooBar") + "|" + to_snake_case("FooBar") + "|" + to_upper_camel_case("foo-bar") + "|" + to_shouty_snake_case("foo bar"))"#.to_string()],
            &mut ctx(&mut v, dir.path()),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("c.txt")).unwrap(),
            "foo-bar|foo_bar|FooBar|FOO_BAR"
        );
    }
}
