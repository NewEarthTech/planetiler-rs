//! Feature rendering: geometry → tiles.
//!
//! For each zoom in a feature's range: project the lon/lat geometry into
//! web-mercator world space, scale it into that zoom's tile-extent space,
//! simplify (Douglas–Peucker, zoom-dependent tolerance), drop it if it is below
//! the minimum on-screen size, then slice it into every covering tile — clip to
//! the tile-plus-buffer rectangle, quantize to the integer grid, fix winding —
//! emitting one [`RenderedFeature`] per covering tile. This is the hardest
//! correctness code in the pipeline.
//!
//! Rendering emits per-feature (bounded by that feature's tile span); the
//! pipeline serializes each emitted feature into a `model::Entry` and sorts them
//! externally, so memory never holds every tile at once.

mod clip;
mod simplify;

use std::collections::BTreeMap;

use geo::BoundingRect;
use geo_types::{Coord, Geometry, LineString, Polygon};
use rayon::prelude::*;

use crate::geom::{TileCoord, lng_lat_to_world};
use crate::model::Value;
use crate::mvt;
use crate::profile::VectorFeature;
use clip::Rect;

/// Number of screen pixels per tile edge (the mercator "world" tile size).
const TILE_PIXELS: f64 = 256.0;

/// Rendering parameters.
#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    /// Lowest zoom to render (inclusive).
    pub min_zoom: u8,
    /// Highest zoom to render (inclusive).
    pub max_zoom: u8,
    /// Tile extent (grid units per edge).
    pub extent: u32,
    /// Tile buffer in extent units.
    pub buffer: f64,
    /// Drop features whose on-screen size is below this many pixels (`0` keeps
    /// everything). Points are never dropped.
    pub min_size_px: f64,
    /// Douglas–Peucker tolerance in pixels (`0` disables simplification).
    pub simplify_tolerance_px: f64,
    /// Only emit tiles intersecting these lon/lat bounds `(west, south, east,
    /// north)`; `None` emits wherever features land.
    pub bounds: Option<(f64, f64, f64, f64)>,
}

/// Geometry reduced to a single kind, with Multi* flattened. Coordinates are in
/// web-mercator world space (`[0, 1]`) after [`extract`], then scaled per zoom
/// into tile-extent space. Y is down; polygon rings carry no repeated closing
/// vertex.
enum RenderGeometry {
    Points(Vec<(f64, f64)>),
    Lines(Vec<Vec<(f64, f64)>>),
    /// Each polygon is its exterior ring followed by holes.
    Polygons(Vec<Vec<Vec<(f64, f64)>>>),
}

/// One feature rendered into a single tile — everything the pipeline needs to
/// build a sort key and serialize the feature blob.
///
/// The layer name and attributes are **borrowed from the source
/// [`VectorFeature`]**, which outlives every piece rendered from it. Only the
/// clipped geometry is per-tile and therefore owned. Owning them instead meant a
/// `String` allocation and a full `BTreeMap` deep clone (one allocation per key,
/// per string value) for *every covering tile* of every feature — copies whose
/// entire life is the next statement, which serializes them into the feature
/// blob and drops them. On a pipeline measured to be allocator-bound
/// (`bench/results/ways-allocator-finding.md`) that was the largest single
/// source of avoidable allocation in the render.
#[derive(Debug, Clone)]
pub struct RenderedFeature<'a> {
    /// Target tile.
    pub coord: TileCoord,
    /// MVT layer name (borrowed from the source feature).
    pub layer: &'a str,
    /// Draw order within the tile+layer.
    pub zorder: i32,
    /// Feature id.
    pub id: u64,
    /// Tile-space geometry.
    pub geometry: mvt::Geometry,
    /// Feature attributes (borrowed from the source feature).
    pub attrs: &'a BTreeMap<String, Value>,
}

/// Render one vector feature into every covering tile across its zoom range
/// (intersected with the options' range), returning one [`RenderedFeature`] per
/// covering tile.
pub fn render_feature<'f>(
    feature: &'f VectorFeature,
    options: &RenderOptions,
) -> Vec<RenderedFeature<'f>> {
    let mut out = Vec::new();
    let Some(world) = extract(&feature.geometry) else {
        return out;
    };
    let min_z = feature.min_zoom.max(options.min_zoom);
    let max_z = feature.max_zoom.min(options.max_zoom);
    for z in min_z..=max_z {
        render_at_zoom(feature, &world, z, options, &mut out);
    }
    out
}

/// Predict, as a cheap over-estimate, how many per-tile pieces
/// [`render_feature`] will emit for this feature — the summed covered-tile-rect
/// area across its zoom range, taken from the raw lng/lat bounding box *before*
/// any simplification, min-size drop, or bounds clip.
///
/// This is the same quantity [`render_at_zoom`] computes as `tile_count`, but
/// derivable per feature *ahead* of the render, which is what lets the pipeline
/// name and bound a monster before it expands. It never under-counts: simplify,
/// min-size, and the bounds clip can each only shrink the covered rect, and
/// clipping can only turn a covered tile empty — so `predict_pieces >=
/// render_feature(..).len()` always holds, making it a safe budget for the
/// render working set. Cost is one bounding-box reduction plus O(zooms)
/// arithmetic, independent of the (possibly enormous) tile count.
///
/// The tile-rect arithmetic is deliberately identical to [`prepare_zoom`]'s
/// (`scale = n * extent`, then `(scaled ± buffer) / extent`): on an unsimplified
/// feature the predicted rect is then bit-for-bit the rect the render derives,
/// not a differently-rounded approximation, so the over-count invariant survives
/// even a coordinate landing exactly on a tile boundary — where a shortcut form
/// (`world * n ± buffer/extent`) could round one tile in and under-count.
pub fn predict_pieces(feature: &VectorFeature, options: &RenderOptions) -> u64 {
    let Some(rect) = feature.geometry.bounding_rect() else {
        return 0;
    };
    // Web-mercator maps lng linearly and lat monotonically to world space, so
    // the world bbox is exactly the transform of the lng/lat corners (NW, SE).
    let (ax, ay) = lng_lat_to_world(rect.min().x, rect.max().y);
    let (bx, by) = lng_lat_to_world(rect.max().x, rect.min().y);
    let (min_wx, max_wx) = (ax.min(bx), ax.max(bx));
    let (min_wy, max_wy) = (ay.min(by), ay.max(by));

    let min_z = feature.min_zoom.max(options.min_zoom);
    let max_z = feature.max_zoom.min(options.max_zoom);
    let extent = f64::from(options.extent);
    let buffer = options.buffer;
    let mut total: u64 = 0;
    for z in min_z..=max_z {
        let n = f64::from(TileCoord::axis_tiles(z));
        let max_tile = TileCoord::axis_tiles(z) - 1;
        // Match `prepare_zoom` exactly: scale the world bbox by `n * extent`,
        // then take `(scaled ± buffer) / extent`. This form is monotonic in the
        // bbox and bit-identical to the render's rect, so simplify / min-size /
        // bounds (all of which only shrink the bbox) keep `predicted >= emitted`.
        let scale = n * extent;
        let tx0 = clamp_tile((min_wx * scale - buffer) / extent, max_tile);
        let tx1 = clamp_tile((max_wx * scale + buffer) / extent, max_tile);
        let ty0 = clamp_tile((min_wy * scale - buffer) / extent, max_tile);
        let ty1 = clamp_tile((max_wy * scale + buffer) / extent, max_tile);
        total += u64::from(tx1 - tx0 + 1) * u64::from(ty1 - ty0 + 1);
    }
    total
}

/// Prepared per-zoom render state shared by the common path ([`render_at_zoom`])
/// and slab-mode ([`render_feature_slabbed`]): the scaled + simplified geometry,
/// the covering tile rect (already clipped to `options.bounds`), and the clip
/// rectangle. Both paths derive their tiles from this one struct, so they emit
/// byte-identical pieces in the identical order.
struct PreparedZoom {
    geometry: RenderGeometry,
    z: u8,
    extent: f64,
    rect: Rect,
    tx0: u32,
    tx1: u32,
    ty0: u32,
    ty1: u32,
}

impl PreparedZoom {
    /// Covering-tile count — drives the parallel-clip decision and the slab budget.
    fn tile_count(&self) -> u64 {
        u64::from(self.tx1 - self.tx0 + 1) * u64::from(self.ty1 - self.ty0 + 1)
    }

    /// The covering tiles in **tx-outer, ty-inner** order — the exact emission
    /// order both the common parallel path and slab-mode preserve.
    fn coords(&self) -> Vec<(u32, u32)> {
        (self.tx0..=self.tx1)
            .flat_map(|tx| (self.ty0..=self.ty1).map(move |ty| (tx, ty)))
            .collect()
    }

    /// Clip the prepared geometry into one tile, or `None` if nothing lands.
    fn piece<'f>(
        &self,
        feature: &'f VectorFeature,
        tx: u32,
        ty: u32,
    ) -> Option<RenderedFeature<'f>> {
        clip_to_tile(&self.geometry, tx, ty, self.extent, &self.rect).map(|tile_geometry| {
            RenderedFeature {
                coord: TileCoord::new(self.z, tx, ty).expect("tile in range"),
                layer: &feature.layer,
                zorder: feature.zorder,
                id: feature.id,
                geometry: tile_geometry,
                attrs: &feature.attrs,
            }
        })
    }
}

/// Scale, simplify, min-size-filter, and compute the (bounds-clipped) covering
/// tile rect for one zoom. `None` when the feature contributes nothing at this
/// zoom (dropped by min-size, or clipped entirely outside the bounds). The
/// single source of truth for the rect + clip that both render paths share.
fn prepare_zoom(world: &RenderGeometry, z: u8, options: &RenderOptions) -> Option<PreparedZoom> {
    let n = f64::from(TileCoord::axis_tiles(z));
    let extent = f64::from(options.extent);
    let px = extent / TILE_PIXELS; // tile-extent units per screen pixel

    // Zoom-dependent thresholds, matching Planetiler: the configured values
    // apply below max zoom; at max zoom both drop to 1/16 px so the final
    // zoom's tiles keep nearly full detail.
    const AT_MAX_ZOOM_PX: f64 = 256.0 / 4096.0;
    let (simplify_px, min_size_px) = if z == options.max_zoom {
        (AT_MAX_ZOOM_PX, AT_MAX_ZOOM_PX)
    } else {
        (options.simplify_tolerance_px, options.min_size_px)
    };

    // World -> this zoom's global tile-extent space.
    let mut geometry = scale_geometry(world, n * extent);
    simplify_geometry(&mut geometry, simplify_px * px);
    if !filter_min_size(&mut geometry, min_size_px * px) {
        return None;
    }

    let buffer = options.buffer;
    let max_tile = TileCoord::axis_tiles(z) - 1;
    let (min_x, min_y, max_x, max_y) = bounds(&geometry);
    let mut tx0 = clamp_tile((min_x - buffer) / extent, max_tile);
    let mut tx1 = clamp_tile((max_x + buffer) / extent, max_tile);
    let mut ty0 = clamp_tile((min_y - buffer) / extent, max_tile);
    let mut ty1 = clamp_tile((max_y + buffer) / extent, max_tile);

    // Clip tile emission to the requested lon/lat bounds (e.g. the source bbox).
    if let Some(clip) = options.bounds {
        let (bx0, by0, bx1, by1) = bounds_tile_range(clip, n, max_tile);
        tx0 = tx0.max(bx0);
        tx1 = tx1.min(bx1);
        ty0 = ty0.max(by0);
        ty1 = ty1.min(by1);
        if tx0 > tx1 || ty0 > ty1 {
            return None;
        }
    }

    let rect = Rect::new(-buffer, -buffer, extent + buffer, extent + buffer);
    Some(PreparedZoom {
        geometry,
        z,
        extent,
        rect,
        tx0,
        tx1,
        ty0,
        ty1,
    })
}

/// A giant feature (admin boundary, coastline) can cover thousands of tiles at
/// high zoom; clipping them serially leaves one rayon task hogging a core while
/// the rest idle. At or above this covering-tile count, clip across cores
/// (nested rayon composes with the per-feature parallelism via work stealing).
/// The ordered collect keeps output identical to the serial loop.
const PARALLEL_TILE_THRESHOLD: u64 = 256;

fn render_at_zoom<'f>(
    feature: &'f VectorFeature,
    world: &RenderGeometry,
    z: u8,
    options: &RenderOptions,
    out: &mut Vec<RenderedFeature<'f>>,
) {
    let Some(plan) = prepare_zoom(world, z, options) else {
        return;
    };
    if plan.tile_count() >= PARALLEL_TILE_THRESHOLD {
        let coords = plan.coords();
        out.par_extend(
            coords
                .into_par_iter()
                .filter_map(|(tx, ty)| plan.piece(feature, tx, ty)),
        );
    } else {
        for tx in plan.tx0..=plan.tx1 {
            for ty in plan.ty0..=plan.ty1 {
                if let Some(rendered) = plan.piece(feature, tx, ty) {
                    out.push(rendered);
                }
            }
        }
    }
}

/// Render one feature in bounded **slabs**, invoking `emit` with each slab's
/// pieces in the exact order [`render_feature`] emits them (zoom ascending,
/// tx-outer, ty-inner), so feeding each slab to the sort in turn reproduces the
/// whole feature's feed byte-for-byte. Peak resident output is one slab
/// (`slab_tiles` covering tiles) regardless of how many tiles the feature
/// covers — the intra-feature bound the batch chunker cannot give a single
/// over-budget monster (measured 8.8 GB for one planet feature). Parallel
/// clipping is kept *within* each slab, so the bound costs resident memory, not
/// the monster's clip throughput. `emit`'s error type is generic so this stays
/// free of the pipeline's error type.
pub fn render_feature_slabbed<'f, E>(
    feature: &'f VectorFeature,
    options: &RenderOptions,
    slab_tiles: usize,
    emit: &mut dyn FnMut(Vec<RenderedFeature<'f>>) -> Result<(), E>,
) -> Result<(), E> {
    let Some(world) = extract(&feature.geometry) else {
        return Ok(());
    };
    let min_z = feature.min_zoom.max(options.min_zoom);
    let max_z = feature.max_zoom.min(options.max_zoom);
    let slab_tiles = slab_tiles.max(1);
    for z in min_z..=max_z {
        let Some(plan) = prepare_zoom(&world, z, options) else {
            continue;
        };
        // `coords` is the same transient the common parallel path builds; slabs
        // are its contiguous sub-ranges, so concatenating their (ordered) clips
        // equals the whole-zoom collect. Empty slabs contribute nothing, exactly
        // as the common path's `filter_map` drops empty tiles.
        let coords = plan.coords();
        for slab in coords.chunks(slab_tiles) {
            let pieces: Vec<RenderedFeature> = slab
                .par_iter()
                .filter_map(|&(tx, ty)| plan.piece(feature, tx, ty))
                .collect();
            if !pieces.is_empty() {
                emit(pieces)?;
            }
        }
    }
    Ok(())
}

// --- projection / extraction -------------------------------------------------

fn world(c: Coord<f64>) -> (f64, f64) {
    lng_lat_to_world(c.x, c.y)
}

fn project_line(line: &LineString<f64>) -> Vec<(f64, f64)> {
    line.0.iter().map(|c| world(*c)).collect()
}

fn project_ring(ring: &LineString<f64>) -> Vec<(f64, f64)> {
    let mut points: Vec<(f64, f64)> = ring.0.iter().map(|c| world(*c)).collect();
    if points.len() >= 2 && points.first() == points.last() {
        points.pop();
    }
    points
}

fn project_polygon(polygon: &Polygon<f64>) -> Vec<Vec<(f64, f64)>> {
    let mut rings = vec![project_ring(polygon.exterior())];
    rings.extend(polygon.interiors().iter().map(project_ring));
    rings
}

fn extract(geometry: &Geometry<f64>) -> Option<RenderGeometry> {
    Some(match geometry {
        Geometry::Point(p) => RenderGeometry::Points(vec![world(p.0)]),
        Geometry::MultiPoint(mp) => {
            RenderGeometry::Points(mp.0.iter().map(|p| world(p.0)).collect())
        }
        Geometry::Line(l) => RenderGeometry::Lines(vec![vec![world(l.start), world(l.end)]]),
        Geometry::LineString(ls) => RenderGeometry::Lines(vec![project_line(ls)]),
        Geometry::MultiLineString(mls) => {
            RenderGeometry::Lines(mls.0.iter().map(project_line).collect())
        }
        Geometry::Polygon(poly) => RenderGeometry::Polygons(vec![project_polygon(poly)]),
        Geometry::MultiPolygon(mp) => {
            RenderGeometry::Polygons(mp.0.iter().map(project_polygon).collect())
        }
        // GeometryCollection / Rect / Triangle are not supported yet.
        _ => return None,
    })
}

// --- per-zoom transform / simplify / min-size --------------------------------

fn scale_geometry(geometry: &RenderGeometry, scale: f64) -> RenderGeometry {
    let s = |(x, y): (f64, f64)| (x * scale, y * scale);
    let scale_path = |path: &Vec<(f64, f64)>| path.iter().map(|&c| s(c)).collect::<Vec<_>>();
    match geometry {
        RenderGeometry::Points(points) => {
            RenderGeometry::Points(points.iter().map(|&c| s(c)).collect())
        }
        RenderGeometry::Lines(lines) => {
            RenderGeometry::Lines(lines.iter().map(scale_path).collect())
        }
        RenderGeometry::Polygons(polygons) => RenderGeometry::Polygons(
            polygons
                .iter()
                .map(|rings| rings.iter().map(scale_path).collect())
                .collect(),
        ),
    }
}

fn simplify_geometry(geometry: &mut RenderGeometry, tolerance: f64) {
    if tolerance < 0.0 {
        return;
    }
    match geometry {
        RenderGeometry::Points(_) => {}
        RenderGeometry::Lines(lines) => {
            for line in lines.iter_mut() {
                *line = simplify::simplify(line, tolerance, false);
            }
        }
        RenderGeometry::Polygons(polygons) => {
            for rings in polygons.iter_mut() {
                for ring in rings.iter_mut() {
                    // Upstream simplifies rings in closed form (first == last)
                    // with anti-collapse; our rings are stored open.
                    let mut closed = std::mem::take(ring);
                    if !closed.is_empty() && closed.first() != closed.last() {
                        closed.push(closed[0]);
                    }
                    let mut simplified = simplify::simplify(&closed, tolerance, true);
                    if simplified.len() >= 2 && simplified.first() == simplified.last() {
                        simplified.pop();
                    }
                    *ring = simplified;
                }
            }
        }
    }
}

/// The feature's representative on-screen size in pixels (line length, or
/// `sqrt(area)` for polygons). Points are treated as always large enough.
/// Apply Planetiler's min-size semantics in place: line parts shorter than
/// `min_units` and polygon rings with area below `min_units²` are dropped
/// (holes included; points are never filtered). Returns `false` when nothing
/// survives.
fn filter_min_size(geometry: &mut RenderGeometry, min_units: f64) -> bool {
    if min_units <= 0.0 {
        return true;
    }
    match geometry {
        RenderGeometry::Points(_) => true,
        RenderGeometry::Lines(lines) => {
            lines.retain(|line| path_length(line) >= min_units);
            !lines.is_empty()
        }
        RenderGeometry::Polygons(polygons) => {
            let min_area = min_units * min_units;
            polygons.retain_mut(|rings| match rings.first() {
                Some(outer) if ring_area(outer).abs() >= min_area => {
                    rings.retain(|ring| ring_area(ring).abs() >= min_area);
                    true
                }
                _ => false,
            });
            !polygons.is_empty()
        }
    }
}

fn path_length(points: &[(f64, f64)]) -> f64 {
    points
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
        .sum()
}

/// Signed area of a ring (positive or negative depending on winding).
fn ring_area(ring: &[(f64, f64)]) -> f64 {
    let n = ring.len();
    let mut area = 0.0;
    for i in 0..n {
        let (x1, y1) = ring[i];
        let (x2, y2) = ring[(i + 1) % n];
        area += x1 * y2 - x2 * y1;
    }
    area / 2.0
}

// --- per-tile clipping / quantization ---------------------------------------

fn clip_to_tile(
    geometry: &RenderGeometry,
    tx: u32,
    ty: u32,
    extent: f64,
    rect: &Rect,
) -> Option<mvt::Geometry> {
    let (ox, oy) = (f64::from(tx) * extent, f64::from(ty) * extent);
    let to_local = |(gx, gy): (f64, f64)| (gx - ox, gy - oy);

    match geometry {
        RenderGeometry::Points(points) => {
            let local: Vec<_> = points.iter().map(|&p| to_local(p)).collect();
            let quantized = quantize_points(&clip::clip_points(&local, rect));
            (!quantized.is_empty()).then_some(mvt::Geometry::Points(quantized))
        }
        RenderGeometry::Lines(lines) => {
            let mut out = Vec::new();
            for line in lines {
                let local: Vec<_> = line.iter().map(|&p| to_local(p)).collect();
                for piece in clip::clip_line(&local, rect) {
                    let quantized = quantize_path(&piece);
                    if quantized.len() >= 2 {
                        out.push(quantized);
                    }
                }
            }
            (!out.is_empty()).then_some(mvt::Geometry::Lines(out))
        }
        RenderGeometry::Polygons(polygons) => {
            let mut out = Vec::new();
            for polygon in polygons {
                for (index, ring) in polygon.iter().enumerate() {
                    let local: Vec<_> = ring.iter().map(|&p| to_local(p)).collect();
                    let clipped = clip::clip_ring(&local, rect);
                    if clipped.len() < 3 {
                        continue;
                    }
                    let mut quantized = quantize_path(&clipped);
                    if quantized.len() >= 2 && quantized.first() == quantized.last() {
                        quantized.pop();
                    }
                    if quantized.len() < 3 {
                        continue;
                    }
                    orient(&mut quantized, index == 0);
                    out.push(quantized);
                }
            }
            (!out.is_empty()).then_some(mvt::Geometry::Polygons(out))
        }
    }
}

/// Java `Math.round`: `floor(x + 0.5)` (differs from Rust's half-away-from-zero
/// on ties/negatives). Matching it keeps the quantized grid at parity.
fn qround(v: f64) -> i32 {
    (v + 0.5).floor() as i32
}

fn quantize_points(points: &[(f64, f64)]) -> Vec<(i32, i32)> {
    points
        .iter()
        .map(|&(x, y)| (qround(x), qround(y)))
        .collect()
}

/// Quantize a path, dropping consecutive duplicate points.
fn quantize_path(points: &[(f64, f64)]) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> = Vec::with_capacity(points.len());
    for &(x, y) in points {
        let p = (qround(x), qround(y));
        if out.last() != Some(&p) {
            out.push(p);
        }
    }
    out
}

/// Twice the signed area (shoelace). Positive = clockwise in Y-down space =
/// an MVT exterior ring; negative = a hole.
fn signed_area2(ring: &[(i32, i32)]) -> i64 {
    let n = ring.len();
    let mut area: i64 = 0;
    for i in 0..n {
        let (x1, y1) = ring[i];
        let (x2, y2) = ring[(i + 1) % n];
        area += x1 as i64 * y2 as i64 - x2 as i64 * y1 as i64;
    }
    area
}

/// Reorient a ring so exteriors have positive area and holes negative.
fn orient(ring: &mut [(i32, i32)], exterior: bool) {
    let area = signed_area2(ring);
    if (exterior && area < 0) || (!exterior && area > 0) {
        ring.reverse();
    }
}

fn bounds(geometry: &RenderGeometry) -> (f64, f64, f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut update = |(x, y): (f64, f64)| {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    };
    match geometry {
        RenderGeometry::Points(points) => points.iter().for_each(|&p| update(p)),
        RenderGeometry::Lines(lines) => lines.iter().flatten().for_each(|&p| update(p)),
        RenderGeometry::Polygons(polygons) => {
            polygons.iter().flatten().flatten().for_each(|&p| update(p));
        }
    }
    (min_x, min_y, max_x, max_y)
}

fn clamp_tile(fractional: f64, max_tile: u32) -> u32 {
    if fractional < 0.0 {
        0
    } else if fractional > f64::from(max_tile) {
        max_tile
    } else {
        fractional as u32
    }
}

/// Tile range `(tx0, ty0, tx1, ty1)` covered by lon/lat `bounds` at a zoom with
/// `n` tiles per axis.
fn bounds_tile_range(bounds: (f64, f64, f64, f64), n: f64, max_tile: u32) -> (u32, u32, u32, u32) {
    let (west, south, east, north) = bounds;
    let (min_wx, min_wy) = lng_lat_to_world(west, north);
    let (max_wx, max_wy) = lng_lat_to_world(east, south);
    (
        clamp_tile(min_wx * n, max_tile),
        clamp_tile(min_wy * n, max_tile),
        clamp_tile(max_wx * n, max_tile),
        clamp_tile(max_wy * n, max_tile),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use geo_types::{
        Coord, Geometry, LineString, MultiLineString, MultiPoint, MultiPolygon, Point, Polygon,
        coord,
    };

    use super::*;
    use crate::geom::world_to_lng_lat;

    /// lon/lat for a world coordinate, so tests can place geometry by tile.
    fn ll(wx: f64, wy: f64) -> (f64, f64) {
        world_to_lng_lat(wx, wy)
    }

    /// Options with min-size and simplification off, for deterministic slicing.
    fn opts(min_zoom: u8, max_zoom: u8) -> RenderOptions {
        RenderOptions {
            min_zoom,
            max_zoom,
            extent: 4096,
            buffer: 0.0,
            min_size_px: 0.0,
            simplify_tolerance_px: 0.0,
            bounds: None,
        }
    }

    fn feature(geometry: Geometry<f64>, min_zoom: u8, max_zoom: u8) -> VectorFeature {
        VectorFeature {
            id: 0,
            layer: "l".to_owned(),
            geometry,
            attrs: BTreeMap::new(),
            min_zoom,
            max_zoom,
            zorder: 0,
        }
    }

    #[test]
    fn point_lands_in_one_tile_inside_extent() {
        let (lng, lat) = ll(0.3, 0.3);
        let f = feature(Geometry::Point(Point::new(lng, lat)), 3, 3);
        let rendered = render_feature(&f, &opts(0, 14));
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].coord.z(), 3);
        match &rendered[0].geometry {
            mvt::Geometry::Points(p) => {
                assert_eq!(p.len(), 1);
                assert!((0..=4096).contains(&p[0].0) && (0..=4096).contains(&p[0].1));
            }
            other => panic!("expected points, got {other:?}"),
        }
    }

    #[test]
    fn line_crossing_boundary_is_sliced_into_two_tiles() {
        let a = ll(0.3, 0.25);
        let b = ll(0.7, 0.25);
        let line = LineString(vec![coord! {x: a.0, y: a.1}, coord! {x: b.0, y: b.1}]);
        let f = feature(Geometry::LineString(line), 1, 1);
        let rendered = render_feature(&f, &opts(0, 14));
        assert_eq!(rendered.len(), 2, "line spans two tiles");
        for r in &rendered {
            assert!(matches!(r.geometry, mvt::Geometry::Lines(_)));
        }
    }

    #[test]
    fn polygon_spanning_four_tiles_is_clipped_into_each() {
        let corners = [(0.25, 0.25), (0.75, 0.25), (0.75, 0.75), (0.25, 0.75)];
        let ring: Vec<_> = corners
            .iter()
            .map(|&(wx, wy)| {
                let (lng, lat) = ll(wx, wy);
                coord! {x: lng, y: lat}
            })
            .collect();
        let f = feature(
            Geometry::Polygon(Polygon::new(LineString(ring), vec![])),
            1,
            1,
        );
        let rendered = render_feature(&f, &opts(0, 14));
        assert_eq!(rendered.len(), 4, "polygon covers all four z1 tiles");
        for r in &rendered {
            match &r.geometry {
                mvt::Geometry::Polygons(rings) => {
                    assert!(!rings.is_empty());
                    assert!(signed_area2(&rings[0]) > 0, "exterior winding");
                }
                other => panic!("expected polygons, got {other:?}"),
            }
        }
    }

    #[test]
    fn zoom_range_is_intersected_with_global() {
        let (lng, lat) = ll(0.3, 0.3);
        let f = feature(Geometry::Point(Point::new(lng, lat)), 2, 5);
        let mut zooms: Vec<u8> = render_feature(&f, &opts(3, 4))
            .iter()
            .map(|r| r.coord.z())
            .collect();
        zooms.sort_unstable();
        assert_eq!(zooms, vec![3, 4]);
    }

    #[test]
    fn min_size_drops_tiny_features_at_low_zoom() {
        // A ~0.0002-world-wide line: sub-pixel at z0, sizeable by z14.
        let a = ll(0.5, 0.5);
        let b = ll(0.5002, 0.5);
        let line = LineString(vec![coord! {x: a.0, y: a.1}, coord! {x: b.0, y: b.1}]);
        let f = feature(Geometry::LineString(line), 0, 14);
        let options = RenderOptions {
            min_size_px: 1.0,
            ..opts(0, 14)
        };
        let zooms: Vec<u8> = render_feature(&f, &options)
            .iter()
            .map(|r| r.coord.z())
            .collect();
        assert!(!zooms.contains(&0), "sub-pixel at z0 is dropped");
        assert!(zooms.contains(&14), "kept at z14");
    }

    #[test]
    fn simplification_reduces_vertices() {
        // A near-straight line densely sampled; simplification collapses it.
        let mut coords = Vec::new();
        for i in 0..=20 {
            let (lng, lat) = ll(0.3 + f64::from(i) * 0.001, 0.5);
            coords.push(coord! {x: lng, y: lat});
        }
        let f = feature(Geometry::LineString(LineString(coords)), 6, 6);
        // Tolerance below 0 is passthrough (0 is active, like upstream).
        let plain = render_feature(
            &f,
            &RenderOptions {
                simplify_tolerance_px: -1.0,
                ..opts(0, 14)
            },
        );
        let simplified = render_feature(
            &f,
            &RenderOptions {
                simplify_tolerance_px: 2.0,
                ..opts(0, 14)
            },
        );
        let count = |rs: &[RenderedFeature]| match &rs[0].geometry {
            mvt::Geometry::Lines(l) => l[0].len(),
            _ => 0,
        };
        assert!(
            count(&simplified) < count(&plain),
            "simplify reduces vertex count"
        );
    }

    #[test]
    fn predicted_pieces_never_under_counts_emitted() {
        // The predictor is a budget for the render working set, so it must never
        // under-count what render_feature actually emits — across geometry
        // kinds, zoom ranges, and the size filters it deliberately ignores.
        let square: Vec<_> = [(0.25, 0.25), (0.75, 0.25), (0.75, 0.75), (0.25, 0.75)]
            .iter()
            .map(|&(wx, wy)| {
                let (lng, lat) = ll(wx, wy);
                coord! {x: lng, y: lat}
            })
            .collect();
        let (pa, pb) = (ll(0.3, 0.3), ll(0.3, 0.25));
        let lb = ll(0.7, 0.25);
        let (ta, tb) = (ll(0.5, 0.5), ll(0.5002, 0.5));

        let cases: Vec<(VectorFeature, RenderOptions)> = vec![
            // Point, single zoom.
            (
                feature(Geometry::Point(Point::new(pa.0, pa.1)), 3, 3),
                opts(0, 14),
            ),
            // Polygon covering four z1 tiles.
            (
                feature(
                    Geometry::Polygon(Polygon::new(LineString(square.clone()), vec![])),
                    1,
                    1,
                ),
                opts(0, 14),
            ),
            // Line sliced across a tile boundary.
            (
                feature(
                    Geometry::LineString(LineString(vec![
                        coord! {x: pb.0, y: pb.1},
                        coord! {x: lb.0, y: lb.1},
                    ])),
                    1,
                    1,
                ),
                opts(0, 14),
            ),
            // Wide polygon over a multi-zoom range.
            (
                feature(
                    Geometry::Polygon(Polygon::new(LineString(square), vec![])),
                    2,
                    6,
                ),
                opts(0, 14),
            ),
            // Sub-pixel line with min-size filtering ON — the predictor ignores
            // the filter, so it must still not under-count the survivors.
            (
                feature(
                    Geometry::LineString(LineString(vec![
                        coord! {x: ta.0, y: ta.1},
                        coord! {x: tb.0, y: tb.1},
                    ])),
                    0,
                    14,
                ),
                RenderOptions {
                    min_size_px: 1.0,
                    ..opts(0, 14)
                },
            ),
        ];

        for (i, (f, options)) in cases.iter().enumerate() {
            let predicted = predict_pieces(f, options);
            let actual = render_feature(f, options).len() as u64;
            assert!(
                predicted >= actual,
                "case {i}: predicted {predicted} < actual {actual}"
            );
        }
    }

    /// Deterministic SplitMix64 — a reproducible case stream with no dev-dep.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Rng(seed)
        }

        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        /// Uniform in `[0, 1)` with a full 53-bit mantissa.
        fn unit(&mut self) -> f64 {
            (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
        }

        /// Uniform inclusive integer in `lo..=hi`.
        fn range(&mut self, lo: u64, hi: u64) -> u64 {
            lo + self.next_u64() % (hi - lo + 1)
        }

        fn boolean(&mut self) -> bool {
            self.next_u64() & 1 == 1
        }
    }

    /// A sorted `(a, b)` pair in `0..=max`, so `a <= b` — a valid zoom range.
    fn sorted_zoom_pair(rng: &mut Rng, max: u8) -> (u8, u8) {
        let a = rng.range(0, u64::from(max)) as u8;
        let b = rng.range(0, u64::from(max)) as u8;
        (a.min(b), a.max(b))
    }

    /// One random lon/lat vertex inside the world-space window, optionally
    /// snapped exactly onto a tile edge at the effective max zoom.
    fn rand_vertex(
        rng: &mut Rng,
        wx0: f64,
        wy0: f64,
        w: f64,
        n_eff: f64,
        snap: bool,
    ) -> Coord<f64> {
        let mut wx = wx0 + rng.unit() * w;
        let mut wy = wy0 + rng.unit() * w;
        if snap {
            wx = (wx * n_eff).round() / n_eff;
            wy = (wy * n_eff).round() / n_eff;
        }
        let (lng, lat) = ll(wx, wy);
        coord! { x: lng, y: lat }
    }

    /// A random feature + render options with bounded tile coverage (so the
    /// actual render stays cheap). `snap` places vertices exactly on tile edges.
    fn random_case(rng: &mut Rng, snap: bool) -> (VectorFeature, RenderOptions) {
        let (fz0, fz1) = sorted_zoom_pair(rng, 14);
        let (oz0, oz1) = sorted_zoom_pair(rng, 14);
        // Size the window to `span` tiles at the effective max zoom; coverage
        // then shrinks geometrically toward low zooms, keeping the render small.
        let eff_max = fz1.min(oz1);
        let span = rng.range(1, 8) as f64;
        let n_eff = f64::from(1u32 << eff_max);
        let w = (span / n_eff).min(0.9);
        let wx0 = rng.unit() * (1.0 - w);
        let wy0 = rng.unit() * (1.0 - w);

        let v = |rng: &mut Rng| rand_vertex(rng, wx0, wy0, w, n_eff, snap);
        let vcount = rng.range(3, 9) as usize;
        let ring = |rng: &mut Rng| -> Vec<Coord<f64>> {
            (0..vcount)
                .map(|_| rand_vertex(rng, wx0, wy0, w, n_eff, snap))
                .collect()
        };
        let geometry = match rng.range(0, 5) {
            0 => Geometry::Point(Point::from(v(rng))),
            1 => Geometry::MultiPoint(MultiPoint::new(
                (0..vcount).map(|_| Point::from(v(rng))).collect(),
            )),
            2 => Geometry::LineString(LineString(ring(rng))),
            3 => Geometry::MultiLineString(MultiLineString(vec![
                LineString(ring(rng)),
                LineString(ring(rng)),
            ])),
            4 => Geometry::Polygon(Polygon::new(LineString(ring(rng)), vec![])),
            _ => Geometry::MultiPolygon(MultiPolygon(vec![
                Polygon::new(LineString(ring(rng)), vec![]),
                Polygon::new(LineString(ring(rng)), vec![]),
            ])),
        };

        let options = RenderOptions {
            min_zoom: oz0,
            max_zoom: oz1,
            extent: if rng.boolean() { 4096 } else { 256 },
            buffer: if rng.boolean() {
                0.0
            } else {
                rng.unit() * 64.0
            },
            min_size_px: if rng.boolean() { 0.0 } else { rng.unit() * 4.0 },
            simplify_tolerance_px: if rng.boolean() { 0.0 } else { rng.unit() * 4.0 },
            bounds: if rng.boolean() {
                None
            } else {
                let a = v(rng);
                let b = v(rng);
                Some((a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y)))
            },
        };
        (feature(geometry, fz0, fz1), options)
    }

    #[test]
    fn predict_pieces_over_counts_random_features() {
        // Fuzz the load-bearing invariant `predict_pieces >= render_feature().len()`
        // across geometry kinds, zoom ranges, buffers, simplify / min-size, and
        // bounds clips. The predictor is the memory budget for the render working
        // set, so a single under-count anywhere would let a monster slip the bound.
        let mut rng = Rng::new(0xB1A5_0F17_C0FF_EE01);
        for i in 0..3000 {
            let (f, options) = random_case(&mut rng, false);
            let predicted = predict_pieces(&f, &options);
            let actual = render_feature(&f, &options).len() as u64;
            assert!(
                predicted >= actual,
                "case {i}: predicted {predicted} < actual {actual}\n  {f:?}\n  {options:?}"
            );
        }
        // Adversarial: vertices snapped exactly onto tile edges at the max zoom —
        // where the pre-fix shortcut arithmetic could floor one tile in and
        // under-count. The `prepare_zoom`-identical form must survive these.
        for i in 0..600 {
            let (f, options) = random_case(&mut rng, true);
            let predicted = predict_pieces(&f, &options);
            let actual = render_feature(&f, &options).len() as u64;
            assert!(
                predicted >= actual,
                "boundary case {i}: predicted {predicted} < actual {actual}\n  {f:?}\n  {options:?}"
            );
        }
    }

    #[test]
    fn predicted_pieces_is_tight_for_solid_coverage() {
        // A polygon that solidly fills its bbox predicts exactly what it emits
        // (the four z1 tiles) — the tightness that keeps the common render path
        // a single chunk once budgeting is layered on top.
        let ring: Vec<_> = [(0.25, 0.25), (0.75, 0.25), (0.75, 0.75), (0.25, 0.75)]
            .iter()
            .map(|&(wx, wy)| {
                let (lng, lat) = ll(wx, wy);
                coord! {x: lng, y: lat}
            })
            .collect();
        let f = feature(
            Geometry::Polygon(Polygon::new(LineString(ring), vec![])),
            1,
            1,
        );
        assert_eq!(predict_pieces(&f, &opts(0, 14)), 4);
        assert_eq!(render_feature(&f, &opts(0, 14)).len(), 4);
    }

    #[test]
    fn slabbed_render_matches_whole_and_bounds_slabs() {
        // Slab-mode must reproduce `render_feature`'s pieces in the identical
        // order (its byte-identity contract) while holding each emitted slab
        // within the tile budget (the bound it exists to enforce). A filled
        // polygon covering a sizeable single-zoom tile rect exercises both.
        let ring: Vec<_> = [(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)]
            .iter()
            .map(|&(wx, wy)| {
                let (lng, lat) = ll(wx, wy);
                coord! {x: lng, y: lat}
            })
            .collect();
        let f = feature(
            Geometry::Polygon(Polygon::new(LineString(ring), vec![])),
            4,
            4,
        );
        let options = opts(0, 14);

        let whole = render_feature(&f, &options);
        assert!(
            whole.len() > 8,
            "test needs a multi-slab feature, got {} pieces",
            whole.len()
        );

        let slab_tiles = 8usize;
        let mut slabs: Vec<Vec<RenderedFeature>> = Vec::new();
        render_feature_slabbed(&f, &options, slab_tiles, &mut |slab| {
            slabs.push(slab);
            Ok::<(), ()>(())
        })
        .unwrap();

        assert!(
            slabs.len() > 1,
            "expected multiple slabs, got {}",
            slabs.len()
        );
        for slab in &slabs {
            assert!(
                slab.len() <= slab_tiles,
                "slab of {} pieces exceeds budget {slab_tiles}",
                slab.len()
            );
        }

        // Concatenated slabs == the whole render, piece for piece, in order.
        let flat: Vec<RenderedFeature> = slabs.into_iter().flatten().collect();
        assert_eq!(flat.len(), whole.len(), "slab piece count matches whole");
        for (a, b) in flat.iter().zip(&whole) {
            assert_eq!(
                format!("{a:?}"),
                format!("{b:?}"),
                "slab piece matches whole piece in order"
            );
        }
    }
}
