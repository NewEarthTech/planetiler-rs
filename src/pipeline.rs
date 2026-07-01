//! In-memory generation pipeline (Phase 2).
//!
//! Ties the stages together: read source features, run them through a
//! [`Profile`] into a [`FeatureCollector`], render each emitted feature into
//! tiles, then encode and write the tiles plus computed metadata to an
//! [`MbtilesWriter`]. The external-sort / worker-parallel path replaces the
//! in-memory grouping in Phase 3.

use std::collections::BTreeMap;
use std::path::Path;

use geo_types::Geometry;

use crate::archive::{
    FieldType, MbtilesWriter, TileArchive, TileArchiveMetadata, TileCompression, VectorLayer,
};
use crate::error::Result;
use crate::model::Value;
use crate::mvt::encode_tile;
use crate::profile::{FeatureCollector, Profile, VectorFeature};
use crate::render::TileAccumulator;
use crate::source::SourceFeature;

/// Generation options (a subset of the eventual CLI surface).
#[derive(Debug, Clone)]
pub struct GenerateOptions {
    /// Lowest zoom to render.
    pub min_zoom: u8,
    /// Highest zoom to render.
    pub max_zoom: u8,
    /// Tile extent (grid units per edge).
    pub extent: u32,
    /// Tile buffer in extent units.
    pub buffer: f64,
    /// Per-tile compression.
    pub compression: TileCompression,
    /// Archive `name` metadata.
    pub name: Option<String>,
    /// Archive `type` metadata (`baselayer` / `overlay`).
    pub layer_type: String,
}

impl Default for GenerateOptions {
    fn default() -> Self {
        Self {
            min_zoom: 0,
            max_zoom: 14,
            extent: 4096,
            buffer: 64.0,
            compression: TileCompression::Gzip,
            name: None,
            layer_type: "overlay".to_owned(),
        }
    }
}

/// Summary of a generation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerateStats {
    /// Number of source features processed.
    pub features: usize,
    /// Number of tiles written.
    pub tiles: usize,
}

/// Run the pipeline: source features → profile → render → MBTiles at `output`.
pub fn generate(
    features: &[SourceFeature],
    profile: &dyn Profile,
    options: &GenerateOptions,
    output: impl AsRef<Path>,
) -> Result<GenerateStats> {
    let mut accumulator = TileAccumulator::new(
        options.min_zoom,
        options.max_zoom,
        options.extent,
        options.buffer,
    );
    let mut layer_stats: BTreeMap<String, LayerStat> = BTreeMap::new();
    let mut bounds = Bounds::default();

    for source in features {
        bounds.add_geometry(&source.geometry);
        let mut collector = FeatureCollector::new(source, options.min_zoom, options.max_zoom);
        profile.process_feature(source, &mut collector);
        for feature in collector.into_features() {
            record_layer_stats(&mut layer_stats, &feature, options);
            accumulator.add(&feature);
        }
    }

    let tiles = accumulator.into_tiles();
    let metadata = build_metadata(&layer_stats, &bounds, options, profile);

    let mut writer = MbtilesWriter::create(output, options.compression)?;
    writer.write_metadata(&metadata)?;
    for (coord, tile) in &tiles {
        writer.write_tile(*coord, &encode_tile(tile))?;
    }
    writer.finish()?;

    Ok(GenerateStats {
        features: features.len(),
        tiles: tiles.len(),
    })
}

/// Accumulated tilejson stats for one layer.
struct LayerStat {
    fields: BTreeMap<String, FieldType>,
    min_zoom: u8,
    max_zoom: u8,
}

fn field_type(value: &Value) -> FieldType {
    match value {
        Value::String(_) => FieldType::String,
        Value::Bool(_) => FieldType::Boolean,
        Value::Int(_) | Value::Uint(_) | Value::Float(_) | Value::Double(_) => FieldType::Number,
    }
}

fn record_layer_stats(
    stats: &mut BTreeMap<String, LayerStat>,
    feature: &VectorFeature,
    options: &GenerateOptions,
) {
    let min_z = feature.min_zoom.max(options.min_zoom);
    let max_z = feature.max_zoom.min(options.max_zoom);
    if min_z > max_z {
        return;
    }
    let entry = stats.entry(feature.layer.clone()).or_insert(LayerStat {
        fields: BTreeMap::new(),
        min_zoom: min_z,
        max_zoom: max_z,
    });
    entry.min_zoom = entry.min_zoom.min(min_z);
    entry.max_zoom = entry.max_zoom.max(max_z);
    for (key, value) in &feature.attrs {
        let ty = field_type(value);
        entry
            .fields
            .entry(key.clone())
            // A key seen with differing types widens to String, like upstream.
            .and_modify(|existing| {
                if *existing != ty {
                    *existing = FieldType::String;
                }
            })
            .or_insert(ty);
    }
}

fn build_metadata(
    layer_stats: &BTreeMap<String, LayerStat>,
    bounds: &Bounds,
    options: &GenerateOptions,
    profile: &dyn Profile,
) -> TileArchiveMetadata {
    let vector_layers = layer_stats
        .iter()
        .map(|(id, stat)| VectorLayer {
            id: id.clone(),
            fields: stat.fields.clone(),
            min_zoom: Some(stat.min_zoom),
            max_zoom: Some(stat.max_zoom),
            description: None,
        })
        .collect();

    let (west, south, east, north) = bounds
        .lng_lat()
        .unwrap_or((-180.0, -85.051_13, 180.0, 85.051_13));
    let center = ((west + east) / 2.0, (south + north) / 2.0, options.min_zoom);

    TileArchiveMetadata {
        name: options
            .name
            .clone()
            .or_else(|| Some(profile.name().to_owned())),
        description: None,
        attribution: None,
        version: None,
        layer_type: options.layer_type.clone(),
        format: "pbf".to_owned(),
        bounds: (west, south, east, north),
        center,
        min_zoom: options.min_zoom,
        max_zoom: options.max_zoom,
        compression: options.compression,
        vector_layers,
    }
}

/// Longitude/latitude bounding box accumulator over source geometry.
#[derive(Debug, Clone)]
struct Bounds {
    min_lng: f64,
    min_lat: f64,
    max_lng: f64,
    max_lat: f64,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            min_lng: f64::INFINITY,
            min_lat: f64::INFINITY,
            max_lng: f64::NEG_INFINITY,
            max_lat: f64::NEG_INFINITY,
        }
    }
}

impl Bounds {
    fn add(&mut self, lng: f64, lat: f64) {
        self.min_lng = self.min_lng.min(lng);
        self.min_lat = self.min_lat.min(lat);
        self.max_lng = self.max_lng.max(lng);
        self.max_lat = self.max_lat.max(lat);
    }

    fn add_geometry(&mut self, geometry: &Geometry<f64>) {
        for_each_coord(geometry, &mut |lng, lat| self.add(lng, lat));
    }

    /// `(west, south, east, north)` if any coordinate was seen.
    fn lng_lat(&self) -> Option<(f64, f64, f64, f64)> {
        if self.min_lng.is_finite() {
            Some((self.min_lng, self.min_lat, self.max_lng, self.max_lat))
        } else {
            None
        }
    }
}

fn for_each_coord(geometry: &Geometry<f64>, f: &mut impl FnMut(f64, f64)) {
    use geo_types::Geometry as G;
    let ring = |ls: &geo_types::LineString<f64>, f: &mut dyn FnMut(f64, f64)| {
        ls.0.iter().for_each(|c| f(c.x, c.y));
    };
    match geometry {
        G::Point(p) => f(p.0.x, p.0.y),
        G::Line(l) => {
            f(l.start.x, l.start.y);
            f(l.end.x, l.end.y);
        }
        G::LineString(ls) => ls.0.iter().for_each(|c| f(c.x, c.y)),
        G::Polygon(poly) => {
            ring(poly.exterior(), f);
            poly.interiors().iter().for_each(|r| ring(r, f));
        }
        G::MultiPoint(mp) => mp.0.iter().for_each(|p| f(p.0.x, p.0.y)),
        G::MultiLineString(mls) => mls
            .0
            .iter()
            .for_each(|ls| ls.0.iter().for_each(|c| f(c.x, c.y))),
        G::MultiPolygon(mp) => mp.0.iter().for_each(|poly| {
            ring(poly.exterior(), f);
            poly.interiors().iter().for_each(|r| ring(r, f));
        }),
        G::GeometryCollection(gc) => gc.0.iter().for_each(|g| for_each_coord(g, f)),
        G::Rect(r) => {
            f(r.min().x, r.min().y);
            f(r.max().x, r.max().y);
        }
        G::Triangle(t) => t.to_array().iter().for_each(|c| f(c.x, c.y)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use geo_types::{Geometry, Point};
    use rusqlite::Connection;

    use super::*;
    use crate::geom::world_to_lng_lat;
    use crate::mvt::decode_tile;
    use crate::profile::PassthroughProfile;

    fn point(id: u64, lng: f64, lat: f64, name: &str) -> SourceFeature {
        SourceFeature {
            id,
            geometry: Geometry::Point(Point::new(lng, lat)),
            tags: BTreeMap::from([("name".to_owned(), Value::from(name))]),
        }
    }

    #[test]
    fn generates_readable_mbtiles_with_metadata() {
        let (lng, lat) = world_to_lng_lat(0.5, 0.5);
        let features = vec![
            point(1, lng, lat, "center"),
            point(2, 7.42, 43.73, "monaco"),
        ];
        let profile = PassthroughProfile::new("poi");
        let options = GenerateOptions {
            min_zoom: 0,
            max_zoom: 3,
            name: Some("t".to_owned()),
            ..Default::default()
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.mbtiles");
        let stats = generate(&features, &profile, &options, &path).unwrap();
        assert_eq!(stats.features, 2);
        assert!(stats.tiles > 0);

        let conn = Connection::open(&path).unwrap();
        let shallow: i64 = conn
            .query_row("select count(*) from tiles_shallow", [], |r| r.get(0))
            .unwrap();
        assert_eq!(shallow as usize, stats.tiles);

        let format: String = conn
            .query_row("select value from metadata where name='format'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(format, "pbf");
        let json: String = conn
            .query_row("select value from metadata where name='json'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            json.contains("\"poi\""),
            "vector_layers mentions the layer: {json}"
        );

        // Decode one tile and confirm the layer is present.
        let blob: Vec<u8> = conn
            .query_row("select tile_data from tiles limit 1", [], |r| r.get(0))
            .unwrap();
        let mut decoder = flate2::read::GzDecoder::new(blob.as_slice());
        let mut raw = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut raw).unwrap();
        let tile = decode_tile(&raw).unwrap();
        assert!(tile.layers.iter().any(|l| l.name == "poi"));
    }
}
