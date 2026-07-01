//! Macro benchmark runner: wall-clock time + OS peak RSS for planetiler-rs
//! versus upstream Planetiler on identical inputs.
//!
//! Phase 0 stands up the measurement plumbing. Because planetiler-rs cannot yet
//! produce tiles, the only wired subcommand is `measure`, which times an
//! arbitrary command — the exact capture the Rust-vs-Java head-to-head will use
//! once both sides can generate a tile. The comparison table and semantic-MVT
//! diff land with the decoder in Phase 1 (see `docs/ARCHITECTURE.md` and
//! `parity/README.md`).

use clap::{Parser, Subcommand};
use planetiler_rs::bench::measure_command;

/// planetiler-rs benchmark harness.
#[derive(Parser)]
#[command(name = "bench", about, version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Time a command and report wall-clock time + peak resident set size.
    ///
    /// Example: `bench measure -- java -Xmx4g -jar planetiler.jar --area=monaco`
    Measure {
        /// Program to run, followed by its arguments (separate with `--`).
        #[arg(required = true, num_args = 1.., trailing_var_arg = true)]
        argv: Vec<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Measure { argv } => {
            let (program, args) = argv
                .split_first()
                .expect("clap guarantees at least one arg");
            let measurement = measure_command(program, args)?;
            println!("{measurement}");
            if !measurement.exit_ok {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
