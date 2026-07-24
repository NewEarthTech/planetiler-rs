//! Pipeline configuration.

use crate::error::{Error, Result};
use crate::geom::MAX_ZOOM;

/// Default MVT tile extent (grid resolution per tile edge).
pub const DEFAULT_TILE_EXTENT: u32 = 4096;
/// Default tile buffer in tile-extent units: features are clipped to the tile
/// plus this margin so lines and labels crossing edges render cleanly.
pub const DEFAULT_BUFFER: f64 = 4.0;
/// Default maximum zoom (matches upstream's OpenMapTiles default).
pub const DEFAULT_MAX_ZOOM: u8 = 14;

/// Global generation configuration threaded through the pipeline.
///
/// Phase 0 pins the shape; per-layer overrides and the full CLI surface are
/// filled in as later phases need them.
#[derive(Debug, Clone)]
pub struct PlanetilerConfig {
    /// Lowest zoom to render (inclusive).
    pub min_zoom: u8,
    /// Highest zoom to render (inclusive).
    pub max_zoom: u8,
    /// Tile extent (grid units per edge); almost always 4096.
    pub tile_extent: u32,
    /// Tile buffer in tile-extent units.
    pub buffer: f64,
}

impl Default for PlanetilerConfig {
    fn default() -> Self {
        Self {
            min_zoom: 0,
            max_zoom: DEFAULT_MAX_ZOOM,
            tile_extent: DEFAULT_TILE_EXTENT,
            buffer: DEFAULT_BUFFER,
        }
    }
}

impl PlanetilerConfig {
    /// Validate zoom bounds against [`MAX_ZOOM`].
    ///
    /// # Errors
    /// Returns [`Error::Config`] if `max_zoom > MAX_ZOOM` or `min_zoom > max_zoom`.
    pub fn validate(&self) -> Result<()> {
        if self.max_zoom > MAX_ZOOM {
            return Err(Error::Config("max_zoom exceeds geom::MAX_ZOOM"));
        }
        if self.min_zoom > self.max_zoom {
            return Err(Error::Config("min_zoom exceeds max_zoom"));
        }
        Ok(())
    }
}

/// Per-layer configuration (min/max zoom overrides and, later, feature limits).
/// Skeleton for now; consumed once real profiles land in Phase 2.
#[derive(Debug, Clone)]
pub struct LayerConfig {
    /// Layer name as it appears in the MVT and in tilejson metadata.
    pub name: String,
    /// Minimum zoom for this layer; falls back to the global minimum if `None`.
    pub min_zoom: Option<u8>,
    /// Maximum zoom for this layer; falls back to the global maximum if `None`.
    pub max_zoom: Option<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let c = PlanetilerConfig::default();
        assert_eq!(c.min_zoom, 0);
        assert_eq!(c.max_zoom, DEFAULT_MAX_ZOOM);
        assert_eq!(c.tile_extent, DEFAULT_TILE_EXTENT);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn validate_rejects_bad_zooms() {
        let mut c = PlanetilerConfig {
            max_zoom: MAX_ZOOM + 1,
            ..Default::default()
        };
        assert!(c.validate().is_err());

        c = PlanetilerConfig {
            min_zoom: 10,
            max_zoom: 5,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }
}
