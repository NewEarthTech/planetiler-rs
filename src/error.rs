//! Crate-wide error type.

use thiserror::Error;

/// Errors produced across planetiler-rs subsystems.
#[derive(Debug, Error)]
pub enum Error {
    /// An invalid tile coordinate was constructed or requested.
    #[error("invalid tile coordinate z{z}/{x}/{y}: {reason}")]
    InvalidTileCoord {
        /// Zoom level.
        z: u8,
        /// Tile column.
        x: u32,
        /// Tile row.
        y: u32,
        /// Why the coordinate was rejected.
        reason: &'static str,
    },

    /// A value could not be packed into the fixed-width sort key.
    #[error("sort-key field out of range: {0}")]
    SortKeyRange(&'static str),

    /// Invalid pipeline configuration.
    #[error("invalid configuration: {0}")]
    Config(&'static str),

    /// A malformed MVT geometry command stream was decoded.
    #[error("malformed MVT geometry: {0}")]
    MvtGeometry(&'static str),

    /// A malformed or unexpected MVT tile was decoded.
    #[error("malformed MVT tile: {0}")]
    MvtDecode(&'static str),

    /// A source could not be read or contained unsupported data.
    #[error("source error: {0}")]
    Source(&'static str),

    /// I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// SQLite failure from the MBTiles archive writer.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Convenience alias for results in this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
