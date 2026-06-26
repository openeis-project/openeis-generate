use anyhow::Result;
use clap::Parser;

use openeis_generate::cli::{run, Cli};

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(&cli)
}
