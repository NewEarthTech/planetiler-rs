//! Command-line interface (clap).
//!
//! Flag names and syntax mirror upstream Planetiler where they overlap
//! (`--output`/`--mbtiles`, `--minzoom`/`--maxzoom`, `--tile-compression`,
//! `--force`) so existing invocations translate. Profile-as-subcommand parity
//! arrives with real profiles; today `generate` runs a GeoJSON pass-through.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::archive::TileCompression;
use crate::pipeline::{GenerateOptions, generate_streaming};
use crate::profile::PassthroughProfile;
use crate::source::{NodeStorage, osm_bounds, stream_source};

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
// The CLI is parsed once; the size gap between `Generate` and `Version` is moot.
#[allow(clippy::large_enum_variant)]
pub enum Command {
    /// Generate a tile archive from a GeoJSON source.
    Generate(GenerateArgs),
    /// Print version information.
    Version,
}

/// Flags for `generate`.
#[derive(Debug, Args)]
pub struct GenerateArgs {
    /// Input GeoJSON file.
    #[arg(long)]
    pub source: PathBuf,
    /// Output MBTiles archive.
    #[arg(long, alias = "mbtiles")]
    pub output: PathBuf,
    /// Layer name for the passed-through features.
    #[arg(long, default_value = "features")]
    pub layer: String,
    /// Lowest zoom to render.
    #[arg(long = "minzoom", alias = "min-zoom", default_value_t = 0)]
    pub minzoom: u8,
    /// Highest zoom to render.
    #[arg(long = "maxzoom", alias = "max-zoom", default_value_t = 14)]
    pub maxzoom: u8,
    /// Tile buffer in extent units.
    #[arg(long, default_value_t = 64.0)]
    pub buffer: f64,
    /// Drop features smaller than this many screen pixels per zoom (0 keeps all).
    #[arg(long = "min-size", default_value_t = 1.0)]
    pub min_size: f64,
    /// Douglas–Peucker simplification tolerance in pixels (0 disables).
    #[arg(long, default_value_t = 0.0)]
    pub simplify: f64,
    /// Only emit tiles within these lon/lat bounds `west,south,east,north`.
    /// Defaults to the OSM source's header bbox (like Planetiler); pass `world`
    /// to disable clipping.
    #[arg(long)]
    pub bounds: Option<String>,
    /// Tile compression: `gzip` or `none`.
    #[arg(long = "tile-compression", default_value = "gzip")]
    pub tile_compression: String,
    /// Node-location storage for OSM: `ram` (default, fast), `mmap` (off-heap),
    /// or `sparse` (off-heap, run-compressed — most compact, for planet scale).
    #[arg(long, default_value = "ram")]
    pub storage: String,
    /// Max features kept per tile layer (0 = unlimited). Bounds memory for
    /// low-zoom mega-tiles when rendering large areas at low zoom.
    #[arg(long = "max-features-per-tile", default_value_t = 0)]
    pub max_features_per_tile: usize,
    /// External-sort RAM budget in MB before spilling sorted chunks to disk.
    /// Higher = less disk I/O (faster) at the cost of more memory.
    #[arg(long = "sort-memory-mb", default_value_t = 256)]
    pub sort_memory_mb: usize,
    /// Archive `name` metadata.
    #[arg(long)]
    pub name: Option<String>,
    /// Overwrite the output if it already exists.
    #[arg(long, default_value_t = false)]
    pub force: bool,
}

/// Parse arguments and dispatch. Entry point used by `main`.
pub fn run() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Generate(args)) => run_generate(args),
        Some(Command::Version) | None => {
            println!("planetiler-rs {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

fn run_generate(args: GenerateArgs) -> anyhow::Result<()> {
    let compression = match args.tile_compression.as_str() {
        "gzip" => TileCompression::Gzip,
        "none" => TileCompression::None,
        other => anyhow::bail!("unknown --tile-compression '{other}' (expected gzip or none)"),
    };
    let storage = match args.storage.as_str() {
        "ram" => NodeStorage::Ram,
        "mmap" => NodeStorage::Mmap,
        "sparse" => NodeStorage::Sparse,
        other => anyhow::bail!("unknown --storage '{other}' (expected ram, mmap, or sparse)"),
    };
    if args.minzoom > args.maxzoom {
        anyhow::bail!(
            "--minzoom {} exceeds --maxzoom {}",
            args.minzoom,
            args.maxzoom
        );
    }
    if args.output.exists() {
        if args.force {
            std::fs::remove_file(&args.output)?;
        } else {
            anyhow::bail!(
                "output {} already exists (use --force)",
                args.output.display()
            );
        }
    }

    tracing::info!(source = %args.source.display(), "streaming source");

    // Default tile bounds to the OSM source's header bbox (matching Planetiler);
    // an explicit `--bounds w,s,e,n` overrides, `--bounds world` disables.
    let bounds = match args.bounds.as_deref() {
        Some("world") | Some("planet") => None,
        Some(spec) => Some(parse_bounds(spec)?),
        None if args.source.extension().and_then(|e| e.to_str()) == Some("pbf") => {
            osm_bounds(&args.source)?
        }
        None => None,
    };

    let options = GenerateOptions {
        min_zoom: args.minzoom,
        max_zoom: args.maxzoom,
        extent: 4096,
        buffer: args.buffer,
        compression,
        min_size_px: args.min_size,
        simplify_tolerance_px: args.simplify,
        bounds,
        sort_memory_bytes: args.sort_memory_mb << 20,
        max_features_per_tile: args.max_features_per_tile,
        name: args.name,
        layer_type: "overlay".to_owned(),
    };
    let profile = PassthroughProfile::new(args.layer);
    let stats = generate_streaming(
        |sink| stream_source(&args.source, sink, storage),
        &profile,
        &options,
        &args.output,
    )?;

    println!(
        "Wrote {}: {} tiles from {} features (z{}-{}).",
        args.output.display(),
        stats.tiles,
        stats.features,
        args.minzoom,
        args.maxzoom,
    );
    Ok(())
}

/// Parse a `west,south,east,north` bounds string.
fn parse_bounds(spec: &str) -> anyhow::Result<(f64, f64, f64, f64)> {
    let parts: Result<Vec<f64>, _> = spec.split(',').map(|p| p.trim().parse::<f64>()).collect();
    match parts.ok().as_deref() {
        Some([west, south, east, north]) => Ok((*west, *south, *east, *north)),
        _ => anyhow::bail!("invalid --bounds '{spec}' (want west,south,east,north)"),
    }
}

/// Install a `tracing` subscriber honouring `RUST_LOG` (defaults to `info`).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
