//! openeis-generate: project/template generator configuration layer.
//!
//! Configuration is authored in [KDL](https://kdl.dev). This crate currently
//! ports (and streamlines) cargo-generate's config subsystem:
//!   - [`config`]: per-template config (`openeis.kdl` inside a template)
//!   - [`app_config`]: user-global favorites config (`~/.config/openeis/openeis.kdl`)
//!   - [`vcs`]: the `Vcs` enum (extracted out of cargo-generate's `args` module)

pub mod app_config;
pub mod archive;
pub mod cli;
pub mod conditional;
pub mod config;
pub mod generate;
pub mod git;
pub mod hooks;
pub mod variables;
pub mod vcs;

pub use app_config::{app_config_path, AppConfig, DefaultsConfig, FavoriteConfig};
pub use cli::{Cli, TemplateSource, TemplateSourceKind};
pub use config::{
    Config, ConditionalConfig, HookPhase, HooksConfig, Placeholder, TemplateConfig,
    CONFIG_FILE_NAME,
};
pub use generate::{expand, render_template, GenerationOptions, GenerationStats};
pub use variables::{collect_variables, parse_defines, Variables};
pub use vcs::Vcs;
