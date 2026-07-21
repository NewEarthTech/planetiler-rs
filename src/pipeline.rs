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
use std::sync::mpsc;

use geo::CoordsIter;
use geo_types::Geometry;
use rayon::prelude::*;

use crate::archive::{ArchiveWriter, FieldType, TileArchiveMetadata, TileCompression, VectorLayer};
use crate::collection::ExternalMergeSort;
use crate::error::{Error, Result};
use crate::geom::TileCoord;
use crate::model::{Entry, MAX_LAYER, SortKey, Value};
use crate::mvt::{self, encode_tile};
use crate::profile::{FeatureCollector, Profile, VectorFeature};
use crate::render::{RenderOptions, predict_pieces, render_feature};
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
    let mut render_probe = RenderProbe::new();
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
                flush_render_batch(
                    &mut batch,
                    &render_options,
                    &mut registry,
                    &mut sort,
                    &mut render_probe,
                )?;
            }
            Ok(())
        };
        stream(&mut sink)?;
    }
    flush_render_batch(
        &mut batch,
        &render_options,
        &mut registry,
        &mut sort,
        &mut render_probe,
    )?;
    render_probe.log_summary(true);
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

// Render-flush probe thresholds (instrumentation only; see `RenderProbe`). A
// batch containing a feature whose *predicted* output pieces exceed this is
// warned before the render alloc — naming the monster before it detonates; a
// single flush whose realized *output* exceeds either bound is warned after.
const PRE_RENDER_PIECES_WARN: u64 = 100_000;
const FLUSH_PIECES_WARN: usize = 100_000;
const FLUSH_BYTES_WARN: usize = 256 << 20;
const PROBE_RENDER_SUMMARY_SECS: u64 = 30;

/// Cut the render batch into sub-chunks whose summed *predicted* pieces stay
/// under this, so the resident rendered working set is bounded to ~one sub-chunk
/// rather than a whole 4096-feature batch — the H-A fix, where a cluster of
/// monster multipolygons in one batch rendered concurrently into a multi-GB
/// `Vec` and walled the run. At the tag-heavy monster rate (~7.7 KB/piece,
/// measured on the planet) this caps a sub-chunk near 256 MB; normal batches
/// predict far below it (great-britain averaged ~4k pieces/flush), so they stay
/// a single chunk — today's exact full-batch code path, unchanged. A single
/// feature predicted over budget still renders whole (its own sub-chunk);
/// bounding that intra-feature term is slab-mode's job, a separate step.
const RENDER_CHUNK_PIECES: u64 = 32_768;

/// A batch's input geometry characterized *before* the par-render, so a monster
/// is named in the log ahead of the allocation storm it triggers. `predicted`
/// pieces (summed covered-tile area, [`predict_pieces`]) is the size driver —
/// the falsified probe showed multi-GB flushes from modest-*coord* ocean/ice
/// polygons whose input-coord warning never fired — while input coords are kept
/// only as a cross-reference against the relation-assembly log.
struct BatchProfile {
    features: usize,
    total_input_coords: u64,
    largest_input_id: u64,
    largest_input_coords: usize,
    total_predicted_pieces: u64,
    max_predicted_id: u64,
    max_predicted_pieces: u64,
}

/// Characterize a (sub-)chunk before rendering: summed/peak input coords (for
/// cross-reference) and summed/peak predicted output pieces (the size driver),
/// the latter read from the pre-computed `predicted` slice. `chunk` and
/// `predicted` are parallel and equal length. One pass, negligible beside the
/// render it precedes.
fn profile_batch(chunk: &[VectorFeature], predicted: &[u64]) -> BatchProfile {
    let mut profile = BatchProfile {
        features: chunk.len(),
        total_input_coords: 0,
        largest_input_id: 0,
        largest_input_coords: 0,
        total_predicted_pieces: 0,
        max_predicted_id: 0,
        max_predicted_pieces: 0,
    };
    for (feature, &predicted) in chunk.iter().zip(predicted) {
        let coords = feature.geometry.coords_count();
        profile.total_input_coords += coords as u64;
        if coords > profile.largest_input_coords {
            profile.largest_input_coords = coords;
            profile.largest_input_id = feature.id;
        }
        profile.total_predicted_pieces += predicted;
        if predicted > profile.max_predicted_pieces {
            profile.max_predicted_pieces = predicted;
            profile.max_predicted_id = feature.id;
        }
    }
    profile
}

/// Partition per-feature `predicted` piece counts (in batch order) into
/// contiguous index ranges whose summed prediction stays under `budget`. A
/// feature whose own prediction alone would overflow the budget forms a
/// singleton range (it cannot be split without slab-mode). The ranges cover
/// `0..predicted.len()` in order with no gaps or overlaps, so rendering them in
/// sequence and feeding each to the sort reproduces the exact whole-batch feed
/// order — the property that makes chunking byte-identical to a single collect.
fn chunk_ranges(predicted: &[u64], budget: u64) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut acc = 0u64;
    for (i, &p) in predicted.iter().enumerate() {
        if i > start && acc.saturating_add(p) > budget {
            ranges.push(start..i);
            start = i;
            acc = 0;
        }
        acc = acc.saturating_add(p);
    }
    if start < predicted.len() {
        ranges.push(start..predicted.len());
    }
    ranges
}

/// H-A render-flush instrumentation. Accumulates the per-flush rendered-output
/// size distribution so a long run leaves a trajectory in the log instead of
/// dying silently, and flags any single flush whose rendered working set is
/// large — the batch that, at planet scale, expands a monster multipolygon into
/// the anonymous-heap runaway that walls the write stage. Pure observation: it
/// never touches sort-feed order or contents, so output stays byte-identical.
struct RenderProbe {
    flushes: u64,
    total_pieces: u64,
    total_bytes: u64,
    total_predicted_pieces: u64,
    max_flush_pieces: usize,
    max_flush_bytes: usize,
    max_flush_input_id: u64,
    max_flush_predicted_pieces: u64,
    max_flush_predicted_id: u64,
    last_summary: std::time::Instant,
}

impl RenderProbe {
    fn new() -> Self {
        Self {
            flushes: 0,
            total_pieces: 0,
            total_bytes: 0,
            total_predicted_pieces: 0,
            max_flush_pieces: 0,
            max_flush_bytes: 0,
            max_flush_input_id: 0,
            max_flush_predicted_pieces: 0,
            max_flush_predicted_id: 0,
            last_summary: std::time::Instant::now(),
        }
    }

    /// Record one flush's rendered output against the pre-render `profile` and
    /// emit a throttled running summary. Logging `predicted` beside the realized
    /// pieces is the fidelity receipt: a predicted/actual ratio near 1 means the
    /// budget the fix will chunk on is tight enough to leave the common path a
    /// single chunk; a large ratio means the predictor over-fires and the fix
    /// design needs revisiting — measured before any bounding code is written.
    fn record(&mut self, profile: &BatchProfile, pieces: usize, bytes: usize) {
        self.flushes += 1;
        self.total_pieces += pieces as u64;
        self.total_bytes += bytes as u64;
        self.total_predicted_pieces += profile.total_predicted_pieces;
        if bytes > self.max_flush_bytes {
            self.max_flush_bytes = bytes;
            self.max_flush_pieces = pieces;
            self.max_flush_input_id = profile.largest_input_id;
            self.max_flush_predicted_pieces = profile.max_predicted_pieces;
            self.max_flush_predicted_id = profile.max_predicted_id;
        }
        // A single oversized flush is the H-A signature; log it as it happens,
        // naming the predicted *output* monster (predicted_id, the feature the
        // pre-render warning flagged) beside the realized size — not just the
        // largest-input feature, which the probe proved is the wrong suspect.
        if pieces > FLUSH_PIECES_WARN || bytes > FLUSH_BYTES_WARN {
            tracing::warn!(
                pieces,
                bytes,
                predicted_pieces = profile.max_predicted_pieces,
                predicted_id = profile.max_predicted_id,
                largest_input_id = profile.largest_input_id,
                "render flush: large rendered working set"
            );
        }
        if self.last_summary.elapsed().as_secs() >= PROBE_RENDER_SUMMARY_SECS {
            self.log_summary(false);
            self.last_summary = std::time::Instant::now();
        }
    }

    fn log_summary(&self, final_run: bool) {
        tracing::info!(
            final_run,
            flushes = self.flushes,
            total_pieces = self.total_pieces,
            total_bytes = self.total_bytes,
            total_predicted_pieces = self.total_predicted_pieces,
            max_flush_pieces = self.max_flush_pieces,
            max_flush_bytes = self.max_flush_bytes,
            max_flush_input_id = self.max_flush_input_id,
            max_flush_predicted_pieces = self.max_flush_predicted_pieces,
            max_flush_predicted_id = self.max_flush_predicted_id,
            "render probe summary"
        );
    }
}

/// Render + blob-encode a batch of vector features across cores, intern their
/// layers, and feed them to the sort. Clears the batch.
fn flush_render_batch(
    batch: &mut Vec<VectorFeature>,
    render_options: &RenderOptions,
    registry: &mut LayerRegistry,
    sort: &mut ExternalMergeSort,
    probe: &mut RenderProbe,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    // Predict each feature's rendered pieces once (an over-estimate of what the
    // render emits), then cut the batch into predicted-bounded sub-chunks and
    // render each in turn so the resident `rendered` Vec never spans the whole
    // batch. Sub-chunks are contiguous and in order and each collect preserves
    // order, so the concatenated sort-feed is byte-for-byte identical to a
    // single whole-batch collect.
    let predicted: Vec<u64> = batch
        .iter()
        .map(|feature| predict_pieces(feature, render_options))
        .collect();
    for range in chunk_ranges(&predicted, RENDER_CHUNK_PIECES) {
        render_chunk(
            &batch[range.clone()],
            &predicted[range],
            render_options,
            registry,
            sort,
            probe,
        )?;
    }
    batch.clear();
    Ok(())
}

/// Render one sub-chunk of a batch and feed it to the sort in order. Isolated so
/// its `rendered` Vec is freed before the next sub-chunk starts, which is what
/// bounds peak render memory to one sub-chunk's worth.
fn render_chunk(
    chunk: &[VectorFeature],
    predicted: &[u64],
    render_options: &RenderOptions,
    registry: &mut LayerRegistry,
    sort: &mut ExternalMergeSort,
    probe: &mut RenderProbe,
) -> Result<()> {
    // Probe: characterize the chunk *before* the par-render. A feature whose
    // predicted output pieces are huge is the H-A death signature — the render
    // below expands it into a multi-GB `rendered` Vec across every rayon
    // worker. Logging it before the alloc means "this line, then silence"
    // localizes the wall to this site and names the offending feature id.
    let profile = profile_batch(chunk, predicted);
    if profile.max_predicted_pieces > PRE_RENDER_PIECES_WARN {
        tracing::warn!(
            features = profile.features,
            max_predicted_id = profile.max_predicted_id,
            max_predicted_pieces = profile.max_predicted_pieces,
            total_predicted_pieces = profile.total_predicted_pieces,
            largest_input_id = profile.largest_input_id,
            largest_input_coords = profile.largest_input_coords,
            total_input_coords = profile.total_input_coords,
            "render batch: feature predicted to expand into a huge tile-piece set"
        );
    }
    let rendered: Vec<(TileCoord, String, i32, Vec<u8>)> = chunk
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
    // Probe: characterize the rendered output — the term that piles up as
    // anonymous heap in H-A. `bytes` counts the retained blob + layer-name
    // bytes, the working set that outlives this call until fed to the sort.
    let pieces = rendered.len();
    let bytes: usize = rendered
        .iter()
        .map(|(_, layer, _, blob)| blob.len() + layer.len())
        .sum();
    probe.record(&profile, pieces, bytes);
    for (coord, layer_name, zorder, blob) in rendered {
        let layer = registry.intern(&layer_name)?;
        sort.add(Entry::new(SortKey::new(coord, layer, zorder)?, blob))?;
    }
    Ok(())
}

/// One whole tile's sorted entries, grouped by the merge stage and decoded +
/// encoded by the parallel stage.
struct RawTile {
    coord: TileCoord,
    entries: Vec<Entry>,
}

// Write-stage probe knobs (instrumentation only; see `WriteProbe`).
const WRITE_HEAD_KEYS: usize = 20;
const WRITE_LOG_ENTRIES: u64 = 5_000_000;
const PROBE_WRITE_SECS: u64 = 10;

/// H-B write-stage instrumentation for the merge thread. Dumps the first keys
/// (catches key corruption / unexpected zoom bits instantly), tracks the
/// largest single tile's working set — the term that runs away if a mega-tile
/// materializes — and emits a throttled progress trajectory. `current_bytes`
/// shadows the current tile's kept value bytes incrementally. Pure observation:
/// nothing here changes what is grouped, sent, or written, so output stays
/// byte-identical.
struct WriteProbe {
    entries_consumed: u64,
    tiles_completed: u64,
    batches_sent: u64,
    current_bytes: usize,
    max_tile_entries: usize,
    max_tile_bytes: usize,
    max_tile_coord: Option<TileCoord>,
    head_keys: Vec<(u8, u32, u32, u8)>,
    head_logged: bool,
    last_log: std::time::Instant,
    last_log_entries: u64,
}

impl WriteProbe {
    fn new() -> Self {
        Self {
            entries_consumed: 0,
            tiles_completed: 0,
            batches_sent: 0,
            current_bytes: 0,
            max_tile_entries: 0,
            max_tile_bytes: 0,
            max_tile_coord: None,
            head_keys: Vec::new(),
            head_logged: false,
            last_log: std::time::Instant::now(),
            last_log_entries: 0,
        }
    }

    /// One entry pulled from the sorted stream (kept or dropped).
    fn on_consume(&mut self, key: SortKey) {
        self.entries_consumed += 1;
        if !self.head_logged {
            if self.head_keys.len() < WRITE_HEAD_KEYS {
                self.head_keys
                    .push((key.z(), key.x(), key.y(), key.layer()));
            }
            if self.head_keys.len() >= WRITE_HEAD_KEYS {
                self.log_head_keys();
                self.head_logged = true;
            }
        }
    }

    /// A fresh tile has started; reset the per-tile byte shadow.
    fn on_tile_start(&mut self) {
        self.current_bytes = 0;
    }

    /// An entry was kept into the current tile.
    fn on_keep(&mut self, value_bytes: usize) {
        self.current_bytes += value_bytes;
    }

    /// The current tile finished; fold it into the max-tile tracker.
    fn on_tile_done(&mut self, coord: TileCoord, entries: usize) {
        self.tiles_completed += 1;
        if self.current_bytes > self.max_tile_bytes {
            self.max_tile_bytes = self.current_bytes;
            self.max_tile_entries = entries;
            self.max_tile_coord = Some(coord);
        }
    }

    fn on_batch_sent(&mut self) {
        self.batches_sent += 1;
    }

    /// Throttled progress line (every `WRITE_LOG_ENTRIES` entries or
    /// `PROBE_WRITE_SECS`), including the current tile's live working set.
    fn maybe_log(&mut self, current: &RawTile) {
        // Consult the wall clock only on a coarse entry stride. At planet scale
        // the merge thread pulls billions of entries and the count condition is
        // almost always true, so an unmasked `Instant::elapsed()` per entry is
        // tens of seconds of pure syscall on the serial path. Short-circuiting
        // on the stride keeps the clock call to ~once per 64Ki entries; this is
        // logging-only, so nothing grouped, sent, or written changes.
        const CLOCK_STRIDE: u64 = 1 << 16;
        if self.entries_consumed - self.last_log_entries < WRITE_LOG_ENTRIES
            && (!self.entries_consumed.is_multiple_of(CLOCK_STRIDE)
                || self.last_log.elapsed().as_secs() < PROBE_WRITE_SECS)
        {
            return;
        }
        tracing::info!(
            entries_consumed = self.entries_consumed,
            tiles_completed = self.tiles_completed,
            batches_sent = self.batches_sent,
            cur_z = current.coord.z(),
            cur_x = current.coord.x(),
            cur_y = current.coord.y(),
            cur_entries = current.entries.len(),
            cur_bytes = self.current_bytes,
            max_tile_entries = self.max_tile_entries,
            max_tile_bytes = self.max_tile_bytes,
            "merge progress"
        );
        self.last_log = std::time::Instant::now();
        self.last_log_entries = self.entries_consumed;
    }

    fn log_head_keys(&self) {
        let formatted: Vec<String> = self
            .head_keys
            .iter()
            .map(|(z, x, y, l)| format!("{z}/{x}/{y}#{l}"))
            .collect();
        tracing::info!(
            count = self.head_keys.len(),
            keys = %formatted.join(" "),
            "merge head keys"
        );
    }

    fn finish(&self) {
        if !self.head_logged && !self.head_keys.is_empty() {
            self.log_head_keys();
        }
        let (max_z, max_x, max_y) = self
            .max_tile_coord
            .map(|c| (c.z(), c.x(), c.y()))
            .unwrap_or((0, 0, 0));
        tracing::info!(
            entries_consumed = self.entries_consumed,
            tiles_completed = self.tiles_completed,
            batches_sent = self.batches_sent,
            max_tile_entries = self.max_tile_entries,
            max_tile_bytes = self.max_tile_bytes,
            max_tile_z = max_z,
            max_tile_x = max_x,
            max_tile_y = max_y,
            "merge done (probe)"
        );
    }
}

/// Stream the sorted entries and write finished tiles, overlapping three
/// stages: a merge thread runs the k-way merge and splits the stream into
/// whole tiles at key boundaries (applying the per-tile-layer cap before any
/// blob is decoded), batches of tiles are decoded + post-processed + encoded +
/// compressed across the rayon pool, and the calling thread inserts the
/// finished blobs serially, in merge order — so output is identical to a
/// fully serial write. Bounded channels keep only a few batches in flight,
/// bounding memory regardless of archive size.
fn write_sorted_tiles(
    sort: ExternalMergeSort,
    registry: &LayerRegistry,
    profile: &dyn Profile,
    extent: u32,
    compression: TileCompression,
    max_features_per_tile: usize,
    writer: &mut ArchiveWriter,
) -> Result<usize> {
    // Tiles per encode batch; a batch also flushes past an entry budget so a
    // run of dense tiles can't accumulate unbounded before hand-off.
    const BATCH_TILES: usize = 256;
    const BATCH_ENTRIES: usize = 1 << 18;
    // Batches in flight per hand-off: enough to keep every stage busy, small
    // enough to cap the pipeline's working set.
    const CHANNEL_DEPTH: usize = 2;

    let (raw_tx, raw_rx) = mpsc::sync_channel::<Vec<RawTile>>(CHANNEL_DEPTH);
    let (encoded_tx, encoded_rx) = mpsc::sync_channel::<Vec<(TileCoord, Vec<u8>)>>(CHANNEL_DEPTH);

    std::thread::scope(|scope| {
        // Stage 1: merge + group by tile. Touches only sort keys, never blobs.
        let merger = scope.spawn(move || -> Result<u64> {
            let mut dropped = 0u64;
            let mut batch: Vec<RawTile> = Vec::new();
            let mut batch_entries = 0usize;
            let mut current: Option<RawTile> = None;
            // Kept-feature count for the current tile's current layer. Entries
            // within a tile arrive layer-contiguous (the layer sits above the
            // zorder in the sort key), so one counter implements the cap.
            let mut layer_seen = (0u8, 0usize);
            let mut probe = WriteProbe::new();
            for entry in sort.into_sorted()? {
                let entry = entry?;
                probe.on_consume(entry.key);
                let tile = entry.key.tile();
                if current.as_ref().map(|raw| raw.coord) != Some(tile) {
                    if let Some(done) = current.take() {
                        probe.on_tile_done(done.coord, done.entries.len());
                        batch_entries += done.entries.len();
                        batch.push(done);
                        if batch.len() >= BATCH_TILES || batch_entries >= BATCH_ENTRIES {
                            if raw_tx.send(std::mem::take(&mut batch)).is_err() {
                                // A downstream stage hung up after failing; its
                                // error is the one worth reporting.
                                return Ok(dropped);
                            }
                            probe.on_batch_sent();
                            batch_entries = 0;
                        }
                    }
                    current = Some(RawTile {
                        coord: tile,
                        entries: Vec::new(),
                    });
                    layer_seen = (0, 0);
                    probe.on_tile_start();
                }
                // Cap kept features per tile-layer so a low-zoom mega-tile
                // can't decode millions of features downstream. Entries arrive
                // in sort order, so the first `max_features_per_tile` are kept
                // deterministically and the rest skipped without decoding.
                if max_features_per_tile != 0 {
                    let layer = entry.key.layer();
                    if layer_seen.0 != layer {
                        layer_seen = (layer, 0);
                    }
                    if layer_seen.1 >= max_features_per_tile {
                        dropped += 1;
                        continue;
                    }
                    layer_seen.1 += 1;
                }
                probe.on_keep(entry.value.len());
                let tile_ref = current
                    .as_mut()
                    .expect("current tile is set at the boundary above");
                tile_ref.entries.push(entry);
                probe.maybe_log(tile_ref);
            }
            if let Some(done) = current.take() {
                probe.on_tile_done(done.coord, done.entries.len());
                batch.push(done);
            }
            if !batch.is_empty() {
                let _ = raw_tx.send(batch);
                probe.on_batch_sent();
            }
            probe.finish();
            Ok(dropped)
        });

        // Stage 2: decode + post-process + encode + compress each batch across
        // the rayon pool. The parallel collect preserves tile order.
        let encoder = scope.spawn(move || -> Result<()> {
            let mut batches_done: u64 = 0;
            let mut last_log = std::time::Instant::now();
            while let Ok(batch) = raw_rx.recv() {
                let encoded = batch
                    .into_par_iter()
                    .map(|raw| encode_raw_tile(raw, registry, profile, extent, compression))
                    .collect::<Result<Vec<Option<(TileCoord, Vec<u8>)>>>>()?;
                let encoded: Vec<(TileCoord, Vec<u8>)> = encoded.into_iter().flatten().collect();
                batches_done += 1;
                if last_log.elapsed().as_secs() >= PROBE_WRITE_SECS {
                    tracing::info!(batches_done, "encode progress");
                    last_log = std::time::Instant::now();
                }
                if encoded_tx.send(encoded).is_err() {
                    return Ok(()); // the writer failed; its error wins
                }
            }
            tracing::info!(batches_done, "encode done (probe)");
            Ok(())
        });

        // Stage 3 (this thread): the serial archive writer.
        let mut tiles_written = 0usize;
        let mut write_result: Result<()> = Ok(());
        let mut last_write_log = std::time::Instant::now();
        'writing: while let Ok(batch) = encoded_rx.recv() {
            for (coord, bytes) in batch {
                if let Err(e) = writer.write_tile_compressed(coord, &bytes) {
                    write_result = Err(e);
                    break 'writing; // dropping the receiver unwinds the stages
                }
                tiles_written += 1;
            }
            if last_write_log.elapsed().as_secs() >= PROBE_WRITE_SECS {
                tracing::info!(tiles_written, "write progress");
                last_write_log = std::time::Instant::now();
            }
        }

        let dropped = merger.join().expect("merge stage panicked")?;
        encoder.join().expect("encode stage panicked")?;
        write_result?;
        if dropped > 0 {
            tracing::info!(
                dropped,
                max_features_per_tile,
                "dropped features over per-tile cap"
            );
        }
        Ok(tiles_written)
    })
}

/// Decode one grouped tile's entries into layers, run the profile's tile-space
/// hook, and encode + compress — the per-tile work of the parallel stage.
/// Returns `None` when post-processing leaves the tile empty.
fn encode_raw_tile(
    raw: RawTile,
    registry: &LayerRegistry,
    profile: &dyn Profile,
    extent: u32,
    compression: TileCompression,
) -> Result<Option<(TileCoord, Vec<u8>)>> {
    let mut layers: BTreeMap<u8, Vec<mvt::Feature>> = BTreeMap::new();
    for entry in &raw.entries {
        let blob = decode_blob(&entry.value)?;
        layers
            .entry(entry.key.layer())
            .or_default()
            .push(mvt::Feature {
                id: blob.id,
                geometry: blob.geometry,
                attributes: blob.attrs,
            });
    }
    let mvt_layers: Vec<mvt::Layer> = layers
        .into_iter()
        .map(|(index, features)| {
            let name = registry.name(index);
            // The profile's tile-space hook (line merging, dissolve, drops).
            let features = profile.post_process_layer(name, raw.coord.z(), features);
            mvt::Layer {
                name: name.to_owned(),
                version: 2,
                extent,
                features,
            }
        })
        .filter(|layer| !layer.features.is_empty())
        .collect();
    if mvt_layers.is_empty() {
        return Ok(None);
    }
    let tile = mvt::Tile { layers: mvt_layers };
    let bytes = compression.compress(&encode_tile(&tile))?;
    Ok(Some((raw.coord, bytes)))
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

    #[test]
    fn chunk_ranges_partition_in_order_under_budget() {
        // Empty input yields no ranges.
        assert!(chunk_ranges(&[], 10).is_empty());

        // Everything under budget stays a single chunk — the common path that
        // must remain today's exact full-batch collect.
        assert_eq!(chunk_ranges(&[1, 2, 3, 4], 100), vec![0..4]);

        // A single over-budget feature is its own chunk; neighbours split off it.
        assert_eq!(chunk_ranges(&[3, 100, 3], 10), vec![0..1, 1..2, 2..3]);

        // Greedy fill: accumulate until the next feature would overflow.
        assert_eq!(chunk_ranges(&[4, 4, 4], 10), vec![0..2, 2..3]);
        assert_eq!(chunk_ranges(&[6, 6, 6], 10), vec![0..1, 1..2, 2..3]);

        // Coverage + ordering invariant over a mixed sequence: ranges tile
        // 0..n contiguously with no gaps or overlaps, and any range summing over
        // budget is a single feature (unsplittable without slab-mode).
        let predicted = [5u64, 5, 1, 50, 2, 3, 40, 40, 1];
        let budget = 10;
        let ranges = chunk_ranges(&predicted, budget);
        let mut next = 0;
        for r in &ranges {
            assert_eq!(r.start, next, "no gap or overlap");
            assert!(r.start < r.end, "non-empty range");
            let sum: u64 = predicted[r.clone()].iter().sum();
            assert!(
                sum <= budget || r.len() == 1,
                "range {r:?} sums {sum} over budget {budget} but is not a singleton"
            );
            next = r.end;
        }
        assert_eq!(next, predicted.len(), "ranges cover the whole batch");
    }
}
