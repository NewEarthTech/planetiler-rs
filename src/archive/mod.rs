//! Tile archive writers.
//!
//! A [`TileArchive`] sink consumes encoded (uncompressed MVT) tiles in tile
//! order plus archive metadata, applying its own per-tile compression. The
//! [`MbtilesWriter`] targets the MBTiles format with the compact/dedup schema,
//! `application_id`, TMS Y-flip, and metadata rows that mirror upstream
//! Planetiler for drop-in parity. A PMTiles writer follows in Phase 5.

mod mbtiles;
mod metadata;

pub use mbtiles::MbtilesWriter;
pub use metadata::{FieldType, TileArchiveMetadata, VectorLayer};

use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;

use crate::error::Result;
use crate::geom::TileCoord;

/// Per-tile compression applied before writing to the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileCompression {
    /// gzip (upstream default). Recorded as `compression=gzip` in metadata.
    Gzip,
    /// No compression. Recorded as `compression=none`.
    None,
}

impl TileCompression {
    /// The metadata string value (`gzip` / `none`).
    pub fn as_str(self) -> &'static str {
        match self {
            TileCompression::Gzip => "gzip",
            TileCompression::None => "none",
        }
    }

    /// Apply this compression to raw MVT bytes.
    pub fn compress(self, data: &[u8]) -> Result<Vec<u8>> {
        match self {
            TileCompression::None => Ok(data.to_vec()),
            TileCompression::Gzip => gzip(data),
        }
    }
}

/// gzip bytes at level 6 — the same zlib level upstream uses. Note the gzip
/// envelope is not byte-identical to Java's `Deflater` output (different header
/// and DEFLATE match-finding); the decompressed payload is what we hold at
/// parity.
fn gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(6));
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

/// A sink for encoded tiles plus archive metadata.
pub trait TileArchive {
    /// Write one encoded (uncompressed MVT) tile at `coord`; the archive applies
    /// its own compression.
    fn write_tile(&mut self, coord: TileCoord, mvt: &[u8]) -> Result<()>;

    /// Set a single metadata key/value.
    fn set_metadata(&mut self, key: &str, value: &str) -> Result<()>;

    /// Flush and finalize the archive.
    fn finish(&mut self) -> Result<()>;
}
