use std::path::Path;
use std::str::FromStr;

use anyhow::{anyhow, Result};
use serde::Deserialize;

/// Version control system to initialize after generating a project.
///
/// Ported from cargo-generate's `args::Vcs`. serde uses the bare variant names
/// (`None` / `Git`), so a config writes `vcs "Git"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
pub enum Vcs {
    #[default]
    None,
    Git,
}

impl Vcs {
    pub const fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// Initialize VCS in `project_dir` after generation. `Git` runs `git init`,
    /// stages everything, and makes an initial commit; `None` is a no-op.
    pub fn initialize(
        &self,
        project_dir: &Path,
        branch: Option<&str>,
        force: bool,
    ) -> Result<()> {
        match self {
            Self::None => Ok(()),
            Self::Git => crate::git::init_repo(project_dir, branch, force),
        }
    }
}

impl FromStr for Vcs {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "NONE" => Ok(Self::None),
            "GIT" => Ok(Self::Git),
            _ => Err(anyhow!("Must be one of 'git' or 'none'")),
        }
    }
}
