//! Macro benchmark runner: wall-clock time + OS peak RSS for planetiler-rs
//! versus upstream Planetiler on identical inputs.
//!
//! Phase 0 stands up the measurement plumbing. Because planetiler-rs cannot yet
//! produce tiles, the only wired subcommand is `measure`, which times an
//! arbitrary command — the exact capture the Rust-vs-Java head-to-head will use
//! once both sides can generate a tile. The comparison table and semantic-MVT
//! diff land with the decoder in Phase 1 (see `ARCHITECTURE.md` and
//! `parity/README.md`).

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use planetiler_rs::bench::{compare_archives, measure_command};

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
    /// Semantically compare two MBTiles archives — the parity oracle.
    Compare {
        /// First archive (e.g. planetiler-rs output).
        a: PathBuf,
        /// Second archive (e.g. Planetiler output).
        b: PathBuf,
    },
    /// Render a tile to SVG for visual inspection; two archives overlay
    /// (blue = first, red = second) so divergence is visible.
    Render {
        /// Zoom level.
        z: u8,
        /// Tile column (x).
        x: u32,
        /// Tile row (y, in XYZ convention).
        y: u32,
        /// Output SVG path.
        out: PathBuf,
        /// One or two MBTiles archives to draw.
        #[arg(required = true, num_args = 1..=2)]
        archives: Vec<PathBuf>,
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
        Command::Compare { a, b } => {
            println!("{}", compare_archives(&a, &b)?);
        }
        Command::Render {
            z,
            x,
            y,
            out,
            archives,
        } => {
            let svg = planetiler_rs::bench::render_tiles_svg(&archives, z, x, y)?;
            std::fs::write(&out, svg)?;
            println!("wrote {}", out.display());
        }
    }
    Ok(())
}
