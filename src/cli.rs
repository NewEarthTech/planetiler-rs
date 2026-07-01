//! Command-line interface (clap).
//!
//! Phase 0 provides only a skeleton so the binary builds and the pipeline
//! wiring has a home. Real subcommands (source/output/threads/zoom-range flags
//! driving an end-to-end generate) arrive with the vertical slice in Phase 2.

use clap::{Parser, Subcommand};

/// planetiler-rs command-line interface.
#[derive(Debug, Parser)]
#[command(name = "planetiler-rs", version, about)]
pub struct Cli {
    /// Subcommand to run; defaults to printing version information.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print version information.
    Version,
}

/// Parse arguments and dispatch. Entry point used by `main`.
pub fn run() -> anyhow::Result<()> {
    init_tracing();
    tracing::debug!(
        version = env!("CARGO_PKG_VERSION"),
        "planetiler-rs starting"
    );

    let cli = Cli::parse();
    match cli.command {
        Some(Command::Version) | None => {
            println!("planetiler-rs {}", env!("CARGO_PKG_VERSION"));
        }
    }
    Ok(())
}

/// Install a `tracing` subscriber honouring `RUST_LOG` (defaults to `info`).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Ignore the error if a global subscriber is already installed.
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
