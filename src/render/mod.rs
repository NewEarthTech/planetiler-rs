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
#[derive(Debug, Clone)]
pub struct RenderedFeature {
    /// Target tile.
    pub coord: TileCoord,
    /// MVT layer name.
    pub layer: String,
    /// Draw order within the tile+layer.
    pub zorder: i32,
    /// Feature id.
    pub id: u64,
    /// Tile-space geometry.
    pub geometry: mvt::Geometry,
    /// Feature attributes.
    pub attrs: BTreeMap<String, Value>,
}

/// Render one vector feature into every covering tile across its zoom range
/// (intersected with the options' range), returning one [`RenderedFeature`] per
/// covering tile.
pub fn render_feature(feature: &VectorFeature, options: &RenderOptions) -> Vec<RenderedFeature> {
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
    // `render_at_zoom` derives tile indices as (scaled_coord ± buffer) / extent
    // where scaled_coord = world * n * extent, i.e. world * n ± buffer/extent.
    let buffer_tiles = options.buffer / extent;
    let mut total: u64 = 0;
    for z in min_z..=max_z {
        let n = f64::from(TileCoord::axis_tiles(z));
        let max_tile = TileCoord::axis_tiles(z) - 1;
        let tx0 = clamp_tile(min_wx * n - buffer_tiles, max_tile);
        let tx1 = clamp_tile(max_wx * n + buffer_tiles, max_tile);
        let ty0 = clamp_tile(min_wy * n - buffer_tiles, max_tile);
        let ty1 = clamp_tile(max_wy * n + buffer_tiles, max_tile);
        total += u64::from(tx1 - tx0 + 1) * u64::from(ty1 - ty0 + 1);
    }
    total
}

fn render_at_zoom(
    feature: &VectorFeature,
    world: &RenderGeometry,
    z: u8,
    options: &RenderOptions,
    out: &mut Vec<RenderedFeature>,
) {
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
        return;
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
            return;
        }
    }

    let rect = Rect::new(-buffer, -buffer, extent + buffer, extent + buffer);
    let make = |tx: u32, ty: u32| {
        clip_to_tile(&geometry, tx, ty, extent, &rect).map(|tile_geometry| RenderedFeature {
            coord: TileCoord::new(z, tx, ty).expect("tile in range"),
            layer: feature.layer.clone(),
            zorder: feature.zorder,
            id: feature.id,
            geometry: tile_geometry,
            attrs: feature.attrs.clone(),
        })
    };

    // A giant feature (admin boundary, coastline) can cover thousands of tiles
    // at high zoom; clipping them serially leaves one rayon task hogging a core
    // while the rest idle. Above a threshold, clip across cores (nested rayon
    // composes with the per-feature parallelism via work stealing). The ordered
    // collect keeps output identical to the serial loop.
    const PARALLEL_TILE_THRESHOLD: u64 = 256;
    let tile_count = u64::from(tx1 - tx0 + 1) * u64::from(ty1 - ty0 + 1);
    if tile_count >= PARALLEL_TILE_THRESHOLD {
        let coords: Vec<(u32, u32)> = (tx0..=tx1)
            .flat_map(|tx| (ty0..=ty1).map(move |ty| (tx, ty)))
            .collect();
        out.par_extend(coords.into_par_iter().filter_map(|(tx, ty)| make(tx, ty)));
    } else {
        for tx in tx0..=tx1 {
            for ty in ty0..=ty1 {
                if let Some(rendered) = make(tx, ty) {
                    out.push(rendered);
                }
            }
        }
    }
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

    use geo_types::{Geometry, LineString, Point, Polygon, coord};

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
}
