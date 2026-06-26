//! Application-level (user-global) configuration, ported from cargo-generate's
//! `app_config.rs` but deserialized from KDL.
//!
//! `toml::Value` is replaced with `serde_json::Value`, and `HashMap` with
//! `IndexMap` so favorites/values keep their file order for stable listing.

use std::path::{Path, PathBuf};
use std::fs;

use anyhow::{bail, Context, Result};
use indexmap::IndexMap;
use serde::Deserialize;

use crate::vcs::Vcs;

/// Name of the application configuration file.
pub const CONFIG_FILE_NAME: &str = "openeis.kdl";

#[derive(Deserialize, Default, Debug, PartialEq, Clone)]
pub struct AppConfig {
    pub defaults: Option<DefaultsConfig>,
    pub favorites: Option<IndexMap<String, FavoriteConfig>>,
    pub values: Option<IndexMap<String, serde_json::Value>>,
}

impl AppConfig {
    pub fn get_favorite_cfg(&self, favorite_name: &str) -> Option<&FavoriteConfig> {
        self.favorites.as_ref().and_then(|f| f.get(favorite_name))
    }
}

#[derive(Deserialize, Default, Debug, PartialEq, Clone)]
pub struct FavoriteConfig {
    pub description: Option<String>,
    pub git: Option<String>,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub revision: Option<String>,
    pub subfolder: Option<String>,
    pub path: Option<PathBuf>,
    pub values: Option<IndexMap<String, serde_json::Value>>,
    pub vcs: Option<Vcs>,
    #[serde(default)]
    pub init: bool,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Deserialize, Default, Debug, PartialEq, Clone)]
pub struct DefaultsConfig {
    /// relates to the CLI's ssh-identity option.
    pub ssh_identity: Option<PathBuf>,
}

impl TryFrom<&Path> for AppConfig {
    type Error = anyhow::Error;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        if !path.exists() {
            return Ok(Default::default());
        }

        let cfg = fs::read_to_string(path)?;
        if cfg.trim().is_empty() {
            Ok(Self::default())
        } else {
            kdl::de::from_str(&cfg)
                .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))
        }
    }
}

/// Resolve the application config path: an explicit `--config` path wins,
/// otherwise fall back to `~/.config/openeis/openeis.kdl`.
pub fn app_config_path(path: &Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = path {
        return p
            .canonicalize()
            .with_context(|| format!("config path does not exist: {}", p.display()));
    }

    if let Some(home) = home::home_dir() {
        return Ok(home.join(".config").join("openeis").join(CONFIG_FILE_NAME));
    }

    bail!(
        "Unable to resolve config file path. \
         Pass --config, or place {CONFIG_FILE_NAME} in ~/.config/openeis/"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    fn tmp_dir() -> TempDir {
        tempfile::Builder::new()
            .prefix("openeis-generate-app")
            .tempdir()
            .expect("temp dir")
    }

    #[test]
    fn parses_favorites_and_defaults() {
        let kdl = r#"
defaults {
    ssh_identity "~/.ssh/id_ed25519"
}
favorites {
    my-tmpl {
        description "my template"
        git "https://example.com/t.git"
        branch "main"
        vcs "Git"
        init true
    }
}
"#;
        let tmp = tmp_dir();
        let path = tmp.path().join(CONFIG_FILE_NAME);
        File::create(&path)
            .expect("create")
            .write_all(kdl.as_bytes())
            .expect("write");

        let cfg = AppConfig::try_from(path.as_path()).expect("parse");
        let defaults = cfg.defaults.as_ref().expect("defaults");
        assert_eq!(
            defaults.ssh_identity.as_deref(),
            Some(std::path::Path::new("~/.ssh/id_ed25519"))
        );

        let fav = cfg.get_favorite_cfg("my-tmpl").expect("favorite");
        assert_eq!(fav.git.as_deref(), Some("https://example.com/t.git"));
        assert_eq!(fav.branch.as_deref(), Some("main"));
        assert_eq!(fav.vcs, Some(Vcs::Git));
        assert!(fav.init);
        assert!(!fav.overwrite);
    }

    #[test]
    fn missing_file_yields_default() {
        let tmp = tmp_dir();
        let missing = tmp.path().join("missing.kdl");
        let cfg = AppConfig::try_from(missing.as_path()).expect("ok");
        assert_eq!(cfg, AppConfig::default());
    }
}
