//! Tile-space feature post-processing, mirroring upstream `FeatureMerge`.
//!
//! Utilities for [`Profile::post_process_layer`](crate::profile::Profile::post_process_layer):
//! today [`merge_line_strings`] — group linestring features by attributes,
//! stitch touching segments end-to-end, drop merged lines below a minimum
//! length, and trim detail outside the tile buffer. Polygon dissolve
//! (`mergeOverlappingPolygons`) arrives with a boolean-ops dependency later.

use std::collections::{HashMap, VecDeque};

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
