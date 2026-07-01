//! Source readers.
//!
//! Decode input formats into a common [`SourceFeature`] carrying lon/lat
//! (WGS84) geometry and typed tags. GeoJSON is supported now; the two-pass OSM
//! reader and shapefile/geoparquet follow in later phases.

mod geojson;
mod osm;

pub use geojson::{read_geojson, stream_geojson};
pub use osm::{osm_bounds, read_osm, stream_osm};

use std::collections::BTreeMap;
use std::path::Path;

use geo_types::Geometry;

use crate::error::Result;
use crate::model::Value;

/// A sink that receives source features one at a time, so readers can stream
/// without materializing the whole extract in memory.
pub type FeatureSink<'a> = dyn FnMut(SourceFeature) -> Result<()> + 'a;

/// Which node-location store the OSM reader uses. RAM is fast and the default;
/// mmap is off-heap for extracts whose node store would not fit in memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NodeStorage {
    /// In-RAM node store (default).
    #[default]
    Ram,
    /// Off-heap, memory-mapped node store (planet scale).
    Mmap,
}

/// Stream a source file into `sink`, dispatching by extension: `.pbf` → OSM,
/// otherwise GeoJSON. `storage` selects the OSM node store (ignored otherwise).
pub fn stream_source(path: &Path, sink: &mut FeatureSink, storage: NodeStorage) -> Result<()> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("pbf") => stream_osm(path, sink, storage),
        _ => stream_geojson(path, sink),
    }
}

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
