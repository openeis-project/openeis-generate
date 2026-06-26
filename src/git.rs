//! Git template source.
//!
//! Clones via the system `git` binary (shells out). This is zero-dependency and
//! supports branch/tag/revision and subfolders naturally; private-repo auth
//! relies on the user's git credential helper / SSH config. Swap for `gix` or
//! `git2` behind this same `clone` function if a library is preferred later.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

/// A git ref to check out.
#[derive(Debug, Clone)]
pub enum GitRef {
    Branch(String),
    Tag(String),
    Revision(String),
}

impl GitRef {
    fn label(&self) -> &str {
        match self {
            GitRef::Branch(s) | GitRef::Tag(s) | GitRef::Revision(s) => s,
        }
    }
}

/// Clone `url` into `dest`.
///
/// - branch/tag → shallow clone with `--branch <ref> --depth 1`.
/// - revision (commit) → full clone, then `git checkout <rev>` (a shallow slice
///   of the default branch usually won't contain an arbitrary commit).
/// - no ref → shallow clone of the default branch.
pub fn clone(url: &str, dest: &Path, git_ref: Option<&GitRef>) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.arg("clone").arg("--quiet");
    match git_ref {
        Some(GitRef::Revision(_)) => { /* full clone needed */ }
        Some(r) => {
            cmd.args(["--branch", r.label()]).args(["--depth", "1"]);
        }
        None => {
            cmd.args(["--depth", "1"]);
        }
    }
    cmd.arg(url).arg(dest);
    run(&mut cmd, url)?;

    if let Some(GitRef::Revision(rev)) = git_ref {
        run(
            Command::new("git")
                .arg("-C")
                .arg(dest)
                .arg("checkout")
                .arg("--quiet")
                .arg(rev),
            url,
        )?;
    }
    Ok(())
}

fn run(cmd: &mut Command, url: &str) -> Result<()> {
    let status = cmd
        .status()
        .with_context(|| format!("failed to execute `git` (is it on PATH?) while cloning {url}"))?;
    if !status.success() {
        bail!("`git` exited with {:?} while cloning {url}", status.code());
    }
    Ok(())
}

/// Initialize a git repo in `dir`, stage everything, and make an initial commit.
///
/// A fallback identity is supplied inline (`-c`) **only** where none is already
/// configured, so the user's global git identity is never clobbered. A
/// "nothing to commit" outcome (empty template) is treated as success.
pub fn init_repo(dir: &Path, branch: Option<&str>, force: bool) -> Result<()> {
    let has_git = dir.join(".git").exists();
    if !has_git || force {
        run(
            Command::new("git").arg("-C").arg(dir).args(["init", "--quiet"]),
            "init",
        )?;
    }
    if let Some(b) = branch {
        // Portable on unborn HEAD (unlike `init -b`, which needs git ≥ 2.28).
        let _ = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["symbolic-ref", "HEAD", &format!("refs/heads/{b}")])
            .status();
    }
    run(
        Command::new("git").arg("-C").arg(dir).args(["add", "."]),
        "stage",
    )?;

    // Commit only when something is actually staged. `git diff --cached --quiet`
    // exits 0 when the index is clean (nothing staged) and 1 when there are
    // staged changes — more robust than parsing `git commit`'s (quiet-suppressed)
    // "nothing to commit" message.
    let clean = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["diff", "--cached", "--quiet"])
        .status()?;
    if !clean.success() {
        let mut commit = Command::new("git");
        commit
            .arg("-C")
            .arg(dir)
            .args(["commit", "--quiet", "-m", "Initial commit (openeis-generate)"]);
        if config_get(dir, "user.email").is_none() {
            commit.args(["-c", "user.email=openeis@localhost"]);
        }
        if config_get(dir, "user.name").is_none() {
            commit.args(["-c", "user.name=openeis-generate"]);
        }
        run(&mut commit, "commit")?;
    }
    Ok(())
}

/// Read a git config value (searching inherited config), or None if unset.
fn config_get(dir: &Path, key: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "--get", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    /// Whether a usable `git` is available, so the network/integration tests
    /// are skipped (not failed) in environments without it.
    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// Create a repo with one commit on the default branch; return its temp dir.
    fn make_repo(content: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        let g = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        g(&["init", "--quiet"]);
        g(&["config", "user.email", "t@t.tt"]);
        g(&["config", "user.name", "t"]);
        fs::write(dir.path().join("hello.txt"), content).unwrap();
        g(&["add", "."]);
        g(&["commit", "--quiet", "-m", "init"]);
        dir
    }

    #[test]
    fn clones_default_branch() {
        if !git_available() {
            return;
        }
        let src = make_repo("hi");
        let dest = TempDir::new().unwrap();
        // file:// so git treats it as a remote clone, not a local copy
        let url = format!("file://{}", src.path().display());
        clone(&url, dest.path(), None).unwrap();
        assert_eq!(
            fs::read_to_string(dest.path().join("hello.txt")).unwrap(),
            "hi"
        );
    }

    #[test]
    fn clones_tag() {
        if !git_available() {
            return;
        }
        let src = make_repo("v1");
        // tag the commit
        Command::new("git")
            .arg("-C")
            .arg(src.path())
            .args(["tag", "rel-1"])
            .status()
            .unwrap();
        fs::write(src.path().join("hello.txt"), "v2").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(src.path())
            .args(["commit", "--quiet", "-am", "second"])
            .status()
            .unwrap();

        let dest = TempDir::new().unwrap();
        let url = format!("file://{}", src.path().display());
        clone(&url, dest.path(), Some(&GitRef::Tag("rel-1".into()))).unwrap();
        // tag points at the first commit → "v1", not HEAD's "v2"
        assert_eq!(
            fs::read_to_string(dest.path().join("hello.txt")).unwrap(),
            "v1"
        );
    }

    #[test]
    fn init_repo_creates_initial_commit() {
        if !git_available() {
            return;
        }
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "hi").unwrap();
        init_repo(dir.path(), None, false).unwrap();

        assert!(dir.path().join(".git").exists());
        let out = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["log", "--oneline"])
            .output()
            .unwrap();
        assert!(out.status.success(), "git log should succeed");
        assert!(String::from_utf8_lossy(&out.stdout).contains("Initial commit"));
    }

    #[test]
    fn init_repo_sets_branch_name() {
        if !git_available() {
            return;
        }
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "hi").unwrap();
        init_repo(dir.path(), Some("mainline"), false).unwrap();

        let out = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["symbolic-ref", "--short", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "mainline");
    }

    #[test]
    fn init_repo_empty_dir_is_ok() {
        if !git_available() {
            return;
        }
        let dir = TempDir::new().unwrap();
        // nothing to commit — must not be treated as an error
        init_repo(dir.path(), None, false).expect("init_repo on empty dir should succeed");
        assert!(dir.path().join(".git").exists());
    }
}
