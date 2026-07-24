//! Tile-space feature post-processing, mirroring upstream `FeatureMerge`.
//!
//! Utilities for [`Profile::post_process_layer`](crate::profile::Profile::post_process_layer):
//! [`merge_line_strings`] — group linestring features by attributes, stitch
//! touching segments end-to-end, drop merged lines below a minimum length,
//! and trim detail outside the tile buffer; and
//! [`merge_overlapping_polygons`] — dissolve same-attribute polygons into
//! fewer polygons covering the same area (union via `geo`'s boolean ops),
//! dropping results below a minimum area.

use std::collections::{HashMap, VecDeque};

use geo::BooleanOps;
use geo_types::{Coord, LineString, MultiPolygon, Polygon};

use crate::mvt::{Feature, Geometry};

/// Pixels per tile edge in upstream's post-processing space; our tile
/// coordinates are `extent` units per edge, so pixel thresholds scale by
/// `extent / 256`.
const TILE_PIXELS: f64 = 256.0;

/// Merge a tile layer's linestring features that share identical attributes:
/// touching segments are stitched end-to-end, merged lines shorter than
/// `min_length_px` are dropped (`0` keeps all), and segments entirely outside
/// the tile plus `buffer_px` are trimmed (negative disables trimming).
/// Non-line features pass through unchanged, in their original order, followed
/// by one merged feature per attribute group (ordered by each group's first
/// appearance), carrying the group's first feature id.
pub fn merge_line_strings(
    features: Vec<Feature>,
    min_length_px: f64,
    buffer_px: f64,
    extent: u32,
) -> Vec<Feature> {
    let units = f64::from(extent) / TILE_PIXELS;
    let min_length = min_length_px * units;
    let buffer = buffer_px * units;

    // A group's prototype feature (id + attributes) plus its collected parts.
    type LineGroup = (Feature, Vec<Vec<(i32, i32)>>);

    let mut result: Vec<Feature> = Vec::with_capacity(features.len());
    // Group line features by attributes in first-seen order; everything else
    // passes straight through (matching upstream's groupByAttrs contract).
    let mut groups: Vec<LineGroup> = Vec::new();
    let mut group_index: HashMap<String, usize> = HashMap::new();
    for feature in features {
        match feature.geometry {
            Geometry::Lines(ref lines) => {
                let key = format!("{:?}", feature.attributes);
                match group_index.get(&key) {
                    Some(&i) => {
                        let Geometry::Lines(lines) = feature.geometry else {
                            unreachable!()
                        };
                        groups[i].1.extend(lines);
                    }
                    None => {
                        group_index.insert(key, groups.len());
                        let parts = lines.clone();
                        groups.push((feature, parts));
                    }
                }
            }
            _ => result.push(feature),
        }
    }

    for (prototype, parts) in groups {
        let mut merged = stitch_paths(parts);
        if min_length > 0.0 {
            merged.retain(|path| path_length(path) >= min_length);
        }
        if buffer >= 0.0 {
            merged = merged
                .into_iter()
                .flat_map(|path| trim_outside_tile(&path, buffer, extent))
                .collect();
        }
        if !merged.is_empty() {
            result.push(Feature {
                id: prototype.id,
                geometry: Geometry::Lines(merged),
                attributes: prototype.attributes,
            });
        }
    }
    result
}

/// Dissolve a tile layer's polygon features that share identical attributes:
/// overlapping or touching polygons are unioned into fewer polygons covering
/// the same area, and result polygons with area below `min_area_px2` (square
/// pixels; `0` keeps all) are dropped. Non-polygon features pass through
/// unchanged in their original order, followed by one merged feature per
/// attribute group (ordered by first appearance), carrying the group's first
/// feature id — the analog of upstream's `mergeOverlappingPolygons`.
pub fn merge_overlapping_polygons(
    features: Vec<Feature>,
    min_area_px2: f64,
    extent: u32,
) -> Vec<Feature> {
    let units = f64::from(extent) / TILE_PIXELS;
    let min_area = min_area_px2 * units * units;

    let mut result: Vec<Feature> = Vec::with_capacity(features.len());
    let mut groups: Vec<(Feature, Vec<Polygon<f64>>)> = Vec::new();
    let mut group_index: HashMap<String, usize> = HashMap::new();
    for feature in features {
        match feature.geometry {
            Geometry::Polygons(ref rings) => {
                let polygons = rings_to_geo(rings);
                let key = format!("{:?}", feature.attributes);
                match group_index.get(&key) {
                    Some(&i) => groups[i].1.extend(polygons),
                    None => {
                        group_index.insert(key, groups.len());
                        groups.push((feature, polygons));
                    }
                }
            }
            _ => result.push(feature),
        }
    }

    for (prototype, polygons) in groups {
        if polygons.is_empty() {
            continue;
        }
        let mut union = MultiPolygon(Vec::new());
        for polygon in &polygons {
            union = union.union(&MultiPolygon(vec![polygon.clone()]));
        }
        let rings = geo_to_rings(&union, min_area);
        if !rings.is_empty() {
            result.push(Feature {
                id: prototype.id,
                geometry: Geometry::Polygons(rings),
                attributes: prototype.attributes,
            });
        }
    }
    result
}

/// Interpret a flat MVT ring list (winding conveys nesting: positive shoelace
/// = exterior, negative = hole of the preceding exterior) as `geo` polygons.
fn rings_to_geo(rings: &[Vec<(i32, i32)>]) -> Vec<Polygon<f64>> {
    let mut polygons: Vec<Polygon<f64>> = Vec::new();
    for ring in rings {
        if ring.len() < 3 {
            continue;
        }
        let line = LineString(
            ring.iter()
                .map(|&(x, y)| Coord {
                    x: f64::from(x),
                    y: f64::from(y),
                })
                .collect(),
        );
        if signed_area2(ring) >= 0 {
            polygons.push(Polygon::new(line, vec![]));
        } else if let Some(last) = polygons.last_mut() {
            last.interiors_push(line);
        }
    }
    polygons
}

/// Convert unioned `geo` polygons back to a flat MVT ring list, rounding to
/// the integer grid, restoring the winding convention (exterior positive,
/// holes negative), and dropping polygons below `min_area` (tile units²) or
/// rings that degenerate under rounding.
fn geo_to_rings(union: &MultiPolygon<f64>, min_area: f64) -> Vec<Vec<(i32, i32)>> {
    let round_ring = |line: &LineString<f64>| -> Vec<(i32, i32)> {
        let mut ring: Vec<(i32, i32)> = line
            .0
            .iter()
            .map(|c| (c.x.round() as i32, c.y.round() as i32))
            .collect();
        if ring.len() >= 2 && ring.first() == ring.last() {
            ring.pop(); // rings are stored open
        }
        ring.dedup();
        ring
    };

    let mut rings = Vec::new();
    for polygon in &union.0 {
        let mut exterior = round_ring(polygon.exterior());
        if exterior.len() < 3 {
            continue;
        }
        let area2 = signed_area2(&exterior);
        if min_area > 0.0 && (area2.abs() as f64) / 2.0 < min_area {
            continue;
        }
        if area2 < 0 {
            exterior.reverse();
        }
        rings.push(exterior);
        for interior in polygon.interiors() {
            let mut hole = round_ring(interior);
            if hole.len() < 3 {
                continue;
            }
            if signed_area2(&hole) > 0 {
                hole.reverse();
            }
            rings.push(hole);
        }
    }
    rings
}

/// Twice the signed shoelace area of an open integer ring. Positive =
/// clockwise in Y-down tile space = an MVT exterior; negative = a hole.
fn signed_area2(ring: &[(i32, i32)]) -> i64 {
    let n = ring.len();
    let mut area: i64 = 0;
    for i in 0..n {
        let (x1, y1) = ring[i];
        let (x2, y2) = ring[(i + 1) % n];
        area += i64::from(x1) * i64::from(y2) - i64::from(x2) * i64::from(y1);
    }
    area
}

/// Stitch open paths end-to-end wherever endpoints coincide exactly (merged
/// features share quantized tile coordinates, so equality matching is safe).
/// Deterministic: seeds are taken in input order and candidates consumed
/// first-in-first-out, like the relation ring stitcher.
fn stitch_paths(paths: Vec<Vec<(i32, i32)>>) -> Vec<Vec<(i32, i32)>> {
    let mut slots: Vec<Option<Vec<(i32, i32)>>> = paths
        .into_iter()
        .filter(|p| p.len() >= 2)
        .map(Some)
        .collect();
    let mut by_endpoint: HashMap<(i32, i32), VecDeque<usize>> = HashMap::new();
    for (index, path) in slots.iter().enumerate() {
        let path = path.as_ref().unwrap();
        by_endpoint.entry(path[0]).or_default().push_back(index);
        by_endpoint
            .entry(*path.last().unwrap())
            .or_default()
            .push_back(index);
    }

    let take_next = |by_endpoint: &mut HashMap<(i32, i32), VecDeque<usize>>,
                     slots: &[Option<Vec<(i32, i32)>>],
                     end: (i32, i32)| {
        let candidates = by_endpoint.get_mut(&end)?;
        loop {
            match candidates.front() {
                Some(&candidate) if slots[candidate].is_none() => {
                    candidates.pop_front();
                }
                Some(&candidate) => return Some(candidate),
                None => return None,
            }
        }
    };

    let mut merged = Vec::new();
    for seed in 0..slots.len() {
        let Some(mut path) = slots[seed].take() else {
            continue;
        };
        // Extend forward from the tail, then (by reversing) from the head, so a
        // fragment that continues either end of the seed gets absorbed.
        for _ in 0..2 {
            while let Some(index) = take_next(&mut by_endpoint, &slots, *path.last().unwrap()) {
                let next = slots[index].take().unwrap();
                if next[0] == *path.last().unwrap() {
                    path.extend_from_slice(&next[1..]);
                } else {
                    path.extend(next.iter().rev().skip(1).copied());
                }
            }
            path.reverse();
        }
        merged.push(path);
    }
    merged
}

/// Length of a path in tile-coordinate units.
fn path_length(path: &[(i32, i32)]) -> f64 {
    path.windows(2)
        .map(|w| {
            let dx = f64::from(w[1].0 - w[0].0);
            let dy = f64::from(w[1].1 - w[0].1);
            dx.hypot(dy)
        })
        .sum()
}

/// Remove runs of segments that lie entirely outside the tile bounds plus
/// `buffer` (in tile units), splitting the path where detail is dropped —
/// a port of upstream's `removeDetailOutsideTile`.
fn trim_outside_tile(path: &[(i32, i32)], buffer: f64, extent: u32) -> Vec<Vec<(i32, i32)>> {
    let min = -buffer;
    let max = f64::from(extent) + buffer;
    let intersects = |a: (i32, i32), b: (i32, i32)| {
        let (x0, x1) = (f64::from(a.0.min(b.0)), f64::from(a.0.max(b.0)));
        let (y0, y1) = (f64::from(a.1.min(b.1)), f64::from(a.1.max(b.1)));
        x1 >= min && x0 <= max && y1 >= min && y0 <= max
    };

    let mut output = Vec::new();
    let mut current: Vec<(i32, i32)> = Vec::new();
    let mut was_in = false;
    for window in path.windows(2) {
        let now_in = intersects(window[0], window[1]);
        if now_in || was_in {
            current.push(window[0]);
        } else if !current.is_empty() {
            if current.len() >= 2 {
                output.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
        was_in = now_in;
    }
    if let Some(&last) = path.last()
        && was_in
    {
        current.push(last);
    }
    if current.len() >= 2 {
        output.push(current);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::Value;

    fn line(id: u64, parts: Vec<Vec<(i32, i32)>>, attr: &str) -> Feature {
        Feature {
            id,
            geometry: Geometry::Lines(parts),
            attributes: BTreeMap::from([("kind".to_owned(), Value::from(attr))]),
        }
    }

    fn parts(feature: &Feature) -> &Vec<Vec<(i32, i32)>> {
        match &feature.geometry {
            Geometry::Lines(lines) => lines,
            other => panic!("expected lines, got {other:?}"),
        }
    }

    #[test]
    fn stitches_touching_lines_with_same_attributes() {
        let features = vec![
            line(1, vec![vec![(0, 0), (100, 0)]], "road"),
            line(2, vec![vec![(100, 0), (200, 0)]], "road"),
        ];
        let merged = merge_line_strings(features, 0.0, -1.0, 4096);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, 1);
        assert_eq!(parts(&merged[0]), &vec![vec![(0, 0), (100, 0), (200, 0)]]);
    }

    #[test]
    fn stitches_reversed_continuations_and_head_extensions() {
        // Second segment continues the first but is stored reversed; third
        // attaches at the seed's *head*.
        let features = vec![
            line(1, vec![vec![(100, 0), (200, 0)]], "road"),
            line(2, vec![vec![(300, 0), (200, 0)]], "road"),
            line(3, vec![vec![(0, 0), (100, 0)]], "road"),
        ];
        let merged = merge_line_strings(features, 0.0, -1.0, 4096);
        assert_eq!(merged.len(), 1);
        let path = &parts(&merged[0])[0];
        assert_eq!(path.len(), 4, "one stitched chain: {path:?}");
    }

    #[test]
    fn different_attributes_stay_separate_and_points_pass_through() {
        let point = Feature {
            id: 9,
            geometry: Geometry::Points(vec![(5, 5)]),
            attributes: BTreeMap::new(),
        };
        let features = vec![
            point.clone(),
            line(1, vec![vec![(0, 0), (100, 0)]], "road"),
            line(2, vec![vec![(100, 0), (200, 0)]], "river"),
        ];
        let merged = merge_line_strings(features, 0.0, -1.0, 4096);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].id, 9, "pass-through keeps original order first");
    }

    #[test]
    fn drops_merged_lines_below_min_length() {
        // 100 units = 6.25 px at extent 4096; a 10 px minimum drops it.
        let features = vec![line(1, vec![vec![(0, 0), (100, 0)]], "road")];
        assert!(merge_line_strings(features, 10.0, -1.0, 4096).is_empty());
    }

    fn polygon(id: u64, rings: Vec<Vec<(i32, i32)>>, attr: &str) -> Feature {
        Feature {
            id,
            geometry: Geometry::Polygons(rings),
            attributes: BTreeMap::from([("kind".to_owned(), Value::from(attr))]),
        }
    }

    /// Axis-aligned square with positive (exterior) winding.
    fn square(x: i32, y: i32, size: i32) -> Vec<(i32, i32)> {
        vec![(x, y), (x + size, y), (x + size, y + size), (x, y + size)]
    }

    fn total_area2(feature: &Feature) -> i64 {
        match &feature.geometry {
            Geometry::Polygons(rings) => rings.iter().map(|r| signed_area2(r)).sum(),
            other => panic!("expected polygons, got {other:?}"),
        }
    }

    #[test]
    fn overlapping_same_attribute_squares_dissolve() {
        let features = vec![
            polygon(1, vec![square(0, 0, 100)], "water"),
            polygon(2, vec![square(50, 50, 100)], "water"),
        ];
        let merged = merge_overlapping_polygons(features, 0.0, 4096);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, 1);
        // Union area: 100² + 100² − 50² overlap = 17500 (×2 for shoelace).
        assert_eq!(total_area2(&merged[0]), 2 * 17500);
    }

    #[test]
    fn disjoint_same_attribute_polygons_become_one_multipolygon() {
        let features = vec![
            polygon(1, vec![square(0, 0, 100)], "water"),
            polygon(2, vec![square(500, 500, 100)], "water"),
        ];
        let merged = merge_overlapping_polygons(features, 0.0, 4096);
        assert_eq!(merged.len(), 1);
        match &merged[0].geometry {
            Geometry::Polygons(rings) => {
                assert_eq!(rings.len(), 2);
                assert!(rings.iter().all(|r| signed_area2(r) > 0));
            }
            other => panic!("expected polygons, got {other:?}"),
        }
    }

    #[test]
    fn different_attribute_polygons_stay_separate() {
        let features = vec![
            polygon(1, vec![square(0, 0, 100)], "water"),
            polygon(2, vec![square(50, 50, 100)], "park"),
        ];
        let merged = merge_overlapping_polygons(features, 0.0, 4096);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn drops_polygons_below_min_area() {
        // 10×10 units = 100 units² < 1 px² (256 units² at extent 4096).
        let features = vec![polygon(1, vec![square(0, 0, 10)], "water")];
        assert!(merge_overlapping_polygons(features, 1.0, 4096).is_empty());
    }

    #[test]
    fn holes_survive_the_round_trip() {
        let mut hole = square(100, 100, 200);
        hole.reverse(); // negative winding = hole
        let features = vec![polygon(1, vec![square(0, 0, 400), hole], "water")];
        let merged = merge_overlapping_polygons(features, 0.0, 4096);
        assert_eq!(merged.len(), 1);
        match &merged[0].geometry {
            Geometry::Polygons(rings) => {
                assert_eq!(rings.len(), 2, "outer + hole: {rings:?}");
                assert!(signed_area2(&rings[0]) > 0);
                assert!(signed_area2(&rings[1]) < 0);
            }
            other => panic!("expected polygons, got {other:?}"),
        }
        // Net area: 400² − 200² = 120000 (×2 for shoelace).
        assert_eq!(total_area2(&merged[0]), 2 * 120_000);
    }

    #[test]
    fn trims_detail_far_outside_the_buffer() {
        // Middle excursion far outside the tile splits the line; the crossing
        // segments stay.
        let path = vec![(0, 0), (-9000, 0), (-9000, 500), (0, 500), (100, 500)];
        let features = vec![line(1, vec![path], "road")];
        let merged = merge_line_strings(features, 0.0, 4.0, 4096);
        assert_eq!(merged.len(), 1);
        let out = parts(&merged[0]);
        assert!(
            out.iter().all(|p| p.len() >= 2),
            "no degenerate parts: {out:?}"
        );
        let survives: usize = out.iter().map(Vec::len).sum();
        assert!(survives >= 4, "crossing segments survive: {out:?}");
    }
}
