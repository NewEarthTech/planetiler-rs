//! Generation pipeline: streaming, external-sort, multi-core.
//!
//! Ties the stages together: a streaming source pushes each feature to a sink,
//! which runs it through a [`Profile`] into a [`FeatureCollector`]; batches are
//! rendered into per-tile pieces across cores, serialized into
//! [`model::Entry`](crate::model) records keyed by sort key, and fed to an
//! [`ExternalMergeSort`]. The sorted entries are then streamed, grouped into
//! whole tiles, and encoded + compressed in parallel before the single writer
//! inserts them. Peak memory is bounded by one render batch, the sort buffer,
//! and (for OSM) the node store — never the whole input or all tiles.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use geo_types::Geometry;
use rayon::prelude::*;

use crate::archive::{ArchiveWriter, FieldType, TileArchiveMetadata, TileCompression, VectorLayer};
use crate::collection::ExternalMergeSort;
use crate::error::{Error, Result};
use crate::geom::TileCoord;
use crate::model::{Entry, MAX_LAYER, SortKey, Value};
use crate::mvt::{self, encode_tile};
use crate::profile::{FeatureCollector, Profile, VectorFeature};
use crate::render::{RenderOptions, render_feature};
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
    /// Drop features smaller than this many screen pixels per zoom (`0` keeps all).
    pub min_size_px: f64,
    /// Douglas–Peucker simplification tolerance in pixels (`0` disables it).
    pub simplify_tolerance_px: f64,
    /// Only emit tiles intersecting these lon/lat bounds `(west, south, east,
    /// north)`; `None` emits wherever features land.
    pub bounds: Option<(f64, f64, f64, f64)>,
    /// Approximate RAM budget for the external sort before spilling to disk.
    pub sort_memory_bytes: usize,
    /// Cap on features kept per tile layer (`0` = unlimited). Bounds memory for
    /// low-zoom mega-tiles; excess features (in sort order) are dropped.
    pub max_features_per_tile: usize,
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
            min_size_px: 1.0,
            simplify_tolerance_px: 0.1,
            bounds: None,
            sort_memory_bytes: 64 << 20,
            max_features_per_tile: 0,
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

/// Run the pipeline over a streaming source: features → profile → render →
/// external sort → MBTiles. The `stream` closure pushes each source feature to a
/// sink, so the whole extract is never held in memory — peak RAM is bounded by
/// one render batch, the sort buffer, and (for OSM) the node store.
pub fn generate_streaming<S>(
    stream: S,
    profile: &dyn Profile,
    options: &GenerateOptions,
    output: impl AsRef<Path>,
) -> Result<GenerateStats>
where
    S: FnOnce(&mut dyn FnMut(SourceFeature) -> Result<()>) -> Result<()>,
{
    let mut sort = ExternalMergeSort::new(options.sort_memory_bytes)?;
    let mut registry = LayerRegistry::default();
    let mut layer_stats: BTreeMap<String, LayerStat> = BTreeMap::new();
    let mut bounds = Bounds::default();
    let mut batch: Vec<VectorFeature> = Vec::new();
    let mut feature_count: usize = 0;

    let render_options = RenderOptions {
        min_zoom: options.min_zoom,
        max_zoom: options.max_zoom,
        extent: options.extent,
        buffer: options.buffer,
        min_size_px: options.min_size_px,
        simplify_tolerance_px: options.simplify_tolerance_px,
        bounds: options.bounds,
    };
    const RENDER_BATCH: usize = 4096;

    // The profile pass, stats, and bounds stay serial and cheap; a full batch is
    // rendered + blob-encoded across cores (order preserved for determinism) and
    // fed to the sort.
    let stream_start = std::time::Instant::now();
    {
        let mut sink = |source: SourceFeature| -> Result<()> {
            feature_count += 1;
            bounds.add_geometry(&source.geometry);
            let mut collector = FeatureCollector::new(&source, options.min_zoom, options.max_zoom);
            profile.process_feature(&source, &mut collector);
            for feature in collector.into_features() {
                record_layer_stats(&mut layer_stats, &feature, options);
                batch.push(feature);
            }
            if batch.len() >= RENDER_BATCH {
                flush_render_batch(&mut batch, &render_options, &mut registry, &mut sort)?;
            }
            Ok(())
        };
        stream(&mut sink)?;
    }
    flush_render_batch(&mut batch, &render_options, &mut registry, &mut sort)?;
    tracing::info!(
        elapsed_s = stream_start.elapsed().as_secs_f64(),
        entries = sort.len(),
        "stream + render + sort-feed done"
    );

    let write_start = std::time::Instant::now();
    let metadata = build_metadata(&layer_stats, &bounds, options, profile);
    let mut writer = ArchiveWriter::create(output, options.compression)?;
    writer.write_metadata(&metadata)?;
    let tiles = write_sorted_tiles(
        sort,
        &registry,
        profile,
        options.extent,
        options.compression,
        options.max_features_per_tile,
        &mut writer,
    )?;
    writer.finish()?;
    tracing::info!(
        elapsed_s = write_start.elapsed().as_secs_f64(),
        tiles,
        "merge + encode + write done"
    );

    Ok(GenerateStats {
        features: feature_count,
        tiles,
    })
}

/// Convenience wrapper generating from an in-memory slice of features.
pub fn generate(
    features: &[SourceFeature],
    profile: &dyn Profile,
    options: &GenerateOptions,
    output: impl AsRef<Path>,
) -> Result<GenerateStats> {
    generate_streaming(
        |sink| {
            for feature in features {
                sink(feature.clone())?;
            }
            Ok(())
        },
        profile,
        options,
        output,
    )
}

/// Render + blob-encode a batch of vector features across cores, intern their
/// layers, and feed them to the sort. Clears the batch.
fn flush_render_batch(
    batch: &mut Vec<VectorFeature>,
    render_options: &RenderOptions,
    registry: &mut LayerRegistry,
    sort: &mut ExternalMergeSort,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let rendered: Vec<(TileCoord, String, i32, Vec<u8>)> = batch
        .par_iter()
        .flat_map_iter(|feature| {
            render_feature(feature, render_options)
                .into_iter()
                .map(|r| {
                    let blob = encode_blob(&FeatureBlob {
                        id: r.id,
                        geometry: r.geometry,
                        attrs: r.attrs,
                    })
                    .expect("feature blob encode is infallible for rendered features");
                    (r.coord, r.layer, r.zorder, blob)
                })
        })
        .collect();
    for (coord, layer_name, zorder, blob) in rendered {
        let layer = registry.intern(&layer_name)?;
        sort.add(Entry::new(SortKey::new(coord, layer, zorder)?, blob))?;
    }
    batch.clear();
    Ok(())
}

/// Stream the sorted entries, group consecutive same-tile records into complete
/// tiles, then encode + compress a batch of them in parallel and insert serially.
fn write_sorted_tiles(
    sort: ExternalMergeSort,
    registry: &LayerRegistry,
    profile: &dyn Profile,
    extent: u32,
    compression: TileCompression,
    max_features_per_tile: usize,
    writer: &mut ArchiveWriter,
) -> Result<usize> {
    const WRITE_BATCH: usize = 256;
    let mut tiles_written = 0;
    let mut dropped: u64 = 0;
    let mut current: Option<TileCoord> = None;
    let mut layers: BTreeMap<u8, Vec<mvt::Feature>> = BTreeMap::new();
    let mut batch: Vec<(TileCoord, mvt::Tile)> = Vec::new();

    let finalize = |coord: TileCoord, layers: &mut BTreeMap<u8, Vec<mvt::Feature>>| {
        let mvt_layers: Vec<mvt::Layer> = std::mem::take(layers)
            .into_iter()
            .map(|(index, features)| {
                let name = registry.name(index);
                // The profile's tile-space hook (line merging, dissolve, drops).
                let features = profile.post_process_layer(name, coord.z(), features);
                mvt::Layer {
                    name: name.to_owned(),
                    version: 2,
                    extent,
                    features,
                }
            })
            .filter(|layer| !layer.features.is_empty())
            .collect();
        (coord, mvt::Tile { layers: mvt_layers })
    };

    for entry in sort.into_sorted()? {
        let entry = entry?;
        let tile = entry.key.tile();
        if current != Some(tile) {
            if let Some(previous) = current.take() {
                let (coord, tile_data) = finalize(previous, &mut layers);
                if !tile_data.layers.is_empty() {
                    batch.push((coord, tile_data));
                }
                if batch.len() >= WRITE_BATCH {
                    flush_batch(&mut batch, compression, writer, &mut tiles_written)?;
                }
            }
            current = Some(tile);
        }
        // Cap features per tile-layer so a low-zoom mega-tile can't hold millions
        // of decoded features in memory. Entries arrive in sort order, so this
        // keeps the first `max_features_per_tile` deterministically and skips the
        // rest (including their decode).
        let layer_features = layers.entry(entry.key.layer()).or_default();
        if max_features_per_tile != 0 && layer_features.len() >= max_features_per_tile {
            dropped += 1;
            continue;
        }
        let blob = decode_blob(&entry.value)?;
        layer_features.push(mvt::Feature {
            id: blob.id,
            geometry: blob.geometry,
            attributes: blob.attrs,
        });
    }
    if let Some(previous) = current.take() {
        let (coord, tile_data) = finalize(previous, &mut layers);
        if !tile_data.layers.is_empty() {
            batch.push((coord, tile_data));
        }
    }
    flush_batch(&mut batch, compression, writer, &mut tiles_written)?;
    if dropped > 0 {
        tracing::info!(
            dropped,
            max_features_per_tile,
            "dropped features over per-tile cap"
        );
    }
    Ok(tiles_written)
}

/// Encode + compress a batch of tiles across cores, then insert them serially.
fn flush_batch(
    batch: &mut Vec<(TileCoord, mvt::Tile)>,
    compression: TileCompression,
    writer: &mut ArchiveWriter,
    count: &mut usize,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let compressed: Vec<(TileCoord, Vec<u8>)> = batch
        .par_iter()
        .map(|(coord, tile)| Ok::<_, Error>((*coord, compression.compress(&encode_tile(tile))?)))
        .collect::<Result<Vec<_>>>()?;
    for (coord, bytes) in compressed {
        writer.write_tile_compressed(coord, &bytes)?;
        *count += 1;
    }
    batch.clear();
    Ok(())
}

/// The opaque per-feature payload sorted alongside the sort key.
#[derive(serde::Serialize, serde::Deserialize)]
struct FeatureBlob {
    id: u64,
    geometry: mvt::Geometry,
    attrs: BTreeMap<String, Value>,
}

fn encode_blob(blob: &FeatureBlob) -> Result<Vec<u8>> {
    postcard::to_allocvec(blob).map_err(|_| Error::Codec("feature blob encode failed"))
}

fn decode_blob(bytes: &[u8]) -> Result<FeatureBlob> {
    postcard::from_bytes(bytes).map_err(|_| Error::Codec("feature blob decode failed"))
}

/// Interns layer names to the small indices packed into the sort key.
#[derive(Default)]
struct LayerRegistry {
    names: Vec<String>,
    index: HashMap<String, u8>,
}

impl LayerRegistry {
    fn intern(&mut self, name: &str) -> Result<u8> {
        if let Some(&i) = self.index.get(name) {
            return Ok(i);
        }
        let i = u8::try_from(self.names.len())
            .ok()
            .filter(|&i| i <= MAX_LAYER)
            .ok_or(Error::Config("too many layers (max 64)"))?;
        self.names.push(name.to_owned());
        self.index.insert(name.to_owned(), i);
        Ok(i)
    }

    fn name(&self, index: u8) -> &str {
        &self.names[index as usize]
    }
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
            relations: Vec::new(),
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

    #[test]
    fn spilling_sort_produces_same_output_as_in_memory() {
        // Distinct tiles so counts are deterministic; a tiny sort budget forces
        // disk spills, which must not change the result.
        let features: Vec<_> = (0..200)
            .map(|i| {
                let (lng, lat) = world_to_lng_lat(0.01 + i as f64 * 0.004, 0.5);
                point(i, lng, lat, "p")
            })
            .collect();
        let profile = PassthroughProfile::new("poi");
        let dir = tempfile::tempdir().unwrap();

        let big = GenerateOptions {
            min_zoom: 0,
            max_zoom: 10,
            ..Default::default()
        };
        let small = GenerateOptions {
            sort_memory_bytes: 256,
            ..big.clone()
        };

        let a = generate(&features, &profile, &big, dir.path().join("a.mbtiles")).unwrap();
        let b = generate(&features, &profile, &small, dir.path().join("b.mbtiles")).unwrap();
        assert_eq!(a, b, "spilling must not change tile output");
    }

    #[test]
    fn caps_features_per_tile() {
        // 100 points that all land in the single z0 tile; a cap of 10 must bound
        // that tile's layer to 10 features.
        let features: Vec<_> = (0..100)
            .map(|i| {
                let (lng, lat) = world_to_lng_lat(0.1 + i as f64 * 0.004, 0.5);
                point(i, lng, lat, "p")
            })
            .collect();
        let profile = PassthroughProfile::new("poi");
        let dir = tempfile::tempdir().unwrap();

        let count_poi = |max_features_per_tile: usize, file: &str| -> usize {
            let options = GenerateOptions {
                min_zoom: 0,
                max_zoom: 0,
                max_features_per_tile,
                ..Default::default()
            };
            let path = dir.path().join(file);
            generate(&features, &profile, &options, &path).unwrap();
            let conn = Connection::open(&path).unwrap();
            let blob: Vec<u8> = conn
                .query_row("select tile_data from tiles limit 1", [], |r| r.get(0))
                .unwrap();
            let mut decoder = flate2::read::GzDecoder::new(blob.as_slice());
            let mut raw = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut raw).unwrap();
            let tile = decode_tile(&raw).unwrap();
            tile.layers
                .iter()
                .find(|l| l.name == "poi")
                .unwrap()
                .features
                .len()
        };

        assert_eq!(count_poi(0, "uncapped.mbtiles"), 100, "cap 0 keeps all");
        assert_eq!(
            count_poi(10, "capped.mbtiles"),
            10,
            "cap bounds the tile layer"
        );
    }

    #[test]
    fn post_process_layer_hook_merges_lines() {
        use geo_types::{LineString, coord};

        use crate::profile::FeatureCollector;

        struct MergingProfile;
        impl Profile for MergingProfile {
            fn process_feature(&self, _source: &SourceFeature, features: &mut FeatureCollector) {
                features.add_all_tags("roads");
            }
            fn post_process_layer(
                &self,
                _layer: &str,
                _zoom: u8,
                features: Vec<mvt::Feature>,
            ) -> Vec<mvt::Feature> {
                crate::merge::merge_line_strings(features, 0.0, -1.0, 4096)
            }
        }

        // Two touching lines with identical tags, inside the single z0 tile.
        let road = |id: u64, from: (f64, f64), to: (f64, f64)| {
            let (ax, ay) = world_to_lng_lat(from.0, from.1);
            let (bx, by) = world_to_lng_lat(to.0, to.1);
            SourceFeature {
                id,
                geometry: Geometry::LineString(LineString(vec![
                    coord! {x: ax, y: ay},
                    coord! {x: bx, y: by},
                ])),
                tags: BTreeMap::from([("kind".to_owned(), Value::from("road"))]),
                relations: Vec::new(),
            }
        };
        let features = vec![
            road(1, (0.4, 0.5), (0.5, 0.5)),
            road(2, (0.5, 0.5), (0.6, 0.5)),
        ];
        let options = GenerateOptions {
            min_zoom: 0,
            max_zoom: 0,
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("merged.mbtiles");
        generate(&features, &MergingProfile, &options, &path).unwrap();

        let conn = Connection::open(&path).unwrap();
        let blob: Vec<u8> = conn
            .query_row("select tile_data from tiles limit 1", [], |r| r.get(0))
            .unwrap();
        let mut decoder = flate2::read::GzDecoder::new(blob.as_slice());
        let mut raw = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut raw).unwrap();
        let tile = decode_tile(&raw).unwrap();
        let layer = tile.layers.iter().find(|l| l.name == "roads").unwrap();
        assert_eq!(
            layer.features.len(),
            1,
            "two touching roads merged into one"
        );
        match &layer.features[0].geometry {
            mvt::Geometry::Lines(lines) => {
                assert_eq!(lines.len(), 1);
                assert_eq!(lines[0].len(), 3, "stitched through the shared point");
            }
            other => panic!("expected lines, got {other:?}"),
        }
    }
}
