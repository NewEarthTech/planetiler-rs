//! Feature rendering: geometry → tiles.
//!
//! Projects a profile's lon/lat [`VectorFeature`] into web-mercator world space,
//! then for each zoom in its range slices it into every covering tile: transform
//! to tile-local extent coordinates, clip to the tile-plus-buffer rectangle,
//! quantize to the integer grid, fix polygon winding, and accumulate per-tile
//! MVT features. This is the hardest correctness code in the pipeline.
//!
//! Phase 2 renders in memory (no external sort yet) and does not simplify;
//! zoom-dependent simplification and the `model::Entry`/external-sort path come
//! next.

mod clip;

use std::collections::BTreeMap;

use geo_types::{Coord, Geometry, LineString, Polygon};

use crate::geom::{TileCoord, lng_lat_to_world};
use crate::mvt;
use crate::profile::VectorFeature;
use clip::Rect;

/// Geometry reduced to a single kind, with Multi* flattened. Coordinates are in
/// web-mercator world space (`[0, 1]`), Y down. Polygon rings carry no repeated
/// closing vertex.
enum RenderGeometry {
    Points(Vec<(f64, f64)>),
    Lines(Vec<Vec<(f64, f64)>>),
    /// Each polygon is its exterior ring followed by holes.
    Polygons(Vec<Vec<Vec<(f64, f64)>>>),
}

/// Accumulates rendered MVT features grouped by tile, then layer.
pub struct TileAccumulator {
    min_zoom: u8,
    max_zoom: u8,
    extent: u32,
    buffer: f64,
    tiles: BTreeMap<TileCoord, BTreeMap<String, Vec<(i32, mvt::Feature)>>>,
}

impl TileAccumulator {
    /// Create an accumulator for the given global zoom range, tile extent, and
    /// buffer (in extent units).
    pub fn new(min_zoom: u8, max_zoom: u8, extent: u32, buffer: f64) -> Self {
        Self {
            min_zoom,
            max_zoom,
            extent,
            buffer,
            tiles: BTreeMap::new(),
        }
    }

    /// Render one vector feature into all covering tiles across its zoom range.
    pub fn add(&mut self, feature: &VectorFeature) {
        let Some(geometry) = extract(&feature.geometry) else {
            return;
        };
        let min_z = feature.min_zoom.max(self.min_zoom);
        let max_z = feature.max_zoom.min(self.max_zoom);
        for z in min_z..=max_z {
            self.add_at_zoom(feature, &geometry, z);
        }
    }

    fn add_at_zoom(&mut self, feature: &VectorFeature, geometry: &RenderGeometry, z: u8) {
        let n = f64::from(TileCoord::axis_tiles(z));
        let extent = f64::from(self.extent);
        let buffer_frac = self.buffer / extent;
        let max_tile = TileCoord::axis_tiles(z) - 1;

        let (min_fx, min_fy, max_fx, max_fy) = bbox_tile_fractional(geometry, n);
        let tx0 = clamp_tile(min_fx - buffer_frac, max_tile);
        let tx1 = clamp_tile(max_fx + buffer_frac, max_tile);
        let ty0 = clamp_tile(min_fy - buffer_frac, max_tile);
        let ty1 = clamp_tile(max_fy + buffer_frac, max_tile);

        let rect = Rect::new(
            -self.buffer,
            -self.buffer,
            extent + self.buffer,
            extent + self.buffer,
        );

        for tx in tx0..=tx1 {
            for ty in ty0..=ty1 {
                let Some(mvt_geometry) = clip_to_tile(geometry, n, tx, ty, extent, &rect) else {
                    continue;
                };
                let coord = TileCoord::new(z, tx, ty).expect("tile in range");
                let mvt_feature = mvt::Feature {
                    id: feature.id,
                    geometry: mvt_geometry,
                    attributes: feature.attrs.clone(),
                };
                self.tiles
                    .entry(coord)
                    .or_default()
                    .entry(feature.layer.clone())
                    .or_default()
                    .push((feature.zorder, mvt_feature));
            }
        }
    }

    /// Consume the accumulator, yielding each non-empty tile as an [`mvt::Tile`],
    /// features ordered by z-order within each layer.
    pub fn into_tiles(self) -> Vec<(TileCoord, mvt::Tile)> {
        let TileAccumulator { extent, tiles, .. } = self;
        tiles
            .into_iter()
            .map(|(coord, layers)| {
                let mvt_layers = layers
                    .into_iter()
                    .map(|(name, mut features)| {
                        features.sort_by_key(|(zorder, _)| *zorder);
                        mvt::Layer {
                            name,
                            version: 2,
                            extent,
                            features: features.into_iter().map(|(_, f)| f).collect(),
                        }
                    })
                    .collect();
                (coord, mvt::Tile { layers: mvt_layers })
            })
            .collect()
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

// --- per-tile clipping / quantization ---------------------------------------

fn clip_to_tile(
    geometry: &RenderGeometry,
    n: f64,
    tx: u32,
    ty: u32,
    extent: f64,
    rect: &Rect,
) -> Option<mvt::Geometry> {
    let to_local = |(wx, wy): (f64, f64)| {
        (
            wx.mul_add(n, -(tx as f64)) * extent,
            wy.mul_add(n, -(ty as f64)) * extent,
        )
    };

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

fn bbox_tile_fractional(geometry: &RenderGeometry, n: f64) -> (f64, f64, f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut update = |(wx, wy): (f64, f64)| {
        let (fx, fy) = (wx * n, wy * n);
        min_x = min_x.min(fx);
        min_y = min_y.min(fy);
        max_x = max_x.max(fx);
        max_y = max_y.max(fy);
    };
    match geometry {
        RenderGeometry::Points(points) => points.iter().for_each(|&p| update(p)),
        RenderGeometry::Lines(lines) => {
            lines.iter().flatten().for_each(|&p| update(p));
        }
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
        let mut acc = TileAccumulator::new(0, 14, 4096, 0.0);
        acc.add(&f);
        let tiles = acc.into_tiles();
        assert_eq!(tiles.len(), 1);
        let (coord, tile) = &tiles[0];
        assert_eq!(coord.z(), 3);
        match &tile.layers[0].features[0].geometry {
            mvt::Geometry::Points(p) => {
                assert_eq!(p.len(), 1);
                assert!((0..=4096).contains(&p[0].0) && (0..=4096).contains(&p[0].1));
            }
            other => panic!("expected points, got {other:?}"),
        }
    }

    #[test]
    fn line_crossing_boundary_is_sliced_into_two_tiles() {
        // Horizontal line spanning the x-boundary at z1 (world x 0.3 -> 0.7,
        // y 0.25 well inside the top tiles), buffer 0 for a clean count.
        let a = ll(0.3, 0.25);
        let b = ll(0.7, 0.25);
        let line = LineString(vec![coord! {x: a.0, y: a.1}, coord! {x: b.0, y: b.1}]);
        let f = feature(Geometry::LineString(line), 1, 1);
        let mut acc = TileAccumulator::new(0, 14, 4096, 0.0);
        acc.add(&f);
        let tiles = acc.into_tiles();

        assert_eq!(
            tiles.len(),
            2,
            "line spans two tiles: {:?}",
            tiles
                .iter()
                .map(|(c, _)| (c.x(), c.y()))
                .collect::<Vec<_>>()
        );
        for (_, tile) in &tiles {
            assert!(matches!(
                tile.layers[0].features[0].geometry,
                mvt::Geometry::Lines(_)
            ));
        }
    }

    #[test]
    fn polygon_spanning_four_tiles_is_clipped_into_each() {
        // A square around the z1 center (world 0.5,0.5) covering all four tiles.
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
        let mut acc = TileAccumulator::new(0, 14, 4096, 0.0);
        acc.add(&f);
        let tiles = acc.into_tiles();

        assert_eq!(tiles.len(), 4, "polygon covers all four z1 tiles");
        for (_, tile) in &tiles {
            match &tile.layers[0].features[0].geometry {
                mvt::Geometry::Polygons(rings) => {
                    assert!(!rings.is_empty());
                    // Exterior ring must have positive (clockwise, Y-down) area.
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
        let mut acc = TileAccumulator::new(3, 4, 4096, 0.0); // global 3..=4
        acc.add(&f);
        let zooms: Vec<u8> = acc.into_tiles().iter().map(|(c, _)| c.z()).collect();
        assert_eq!(zooms, vec![3, 4]);
    }
}
