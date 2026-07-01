//! Source readers.
//!
//! Decode input formats into a common [`SourceFeature`] carrying lon/lat
//! (WGS84) geometry and typed tags. GeoJSON is supported now; the two-pass OSM
//! reader and shapefile/geoparquet follow in later phases.

mod geojson;

pub use geojson::read_geojson;

use std::collections::BTreeMap;

use geo_types::Geometry;

use crate::model::Value;

/// A feature read from a source: geometry in lon/lat degrees, plus tags.
#[derive(Debug, Clone)]
pub struct SourceFeature {
    /// Source-assigned id (feature index when the source has none).
    pub id: u64,
    /// Geometry in lon/lat (WGS84) — `x` is longitude, `y` is latitude.
    pub geometry: Geometry<f64>,
    /// Attribute tags.
    pub tags: BTreeMap<String, Value>,
}

impl SourceFeature {
    /// Look up a tag by key.
    pub fn tag(&self, key: &str) -> Option<&Value> {
        self.tags.get(key)
    }
}
