//! OpenStreetMap `.osm.pbf` source reader (nodes + ways).
//!
//! Two passes: pass 1 records every node's location (the in-memory node-location
//! store) and emits tagged nodes as point features; pass 2 assembles ways into
//! line or polygon geometry from those locations. Relations/multipolygons and an
//! off-heap node store come later. The whole extract is held in memory for now
//! (fine for city/region extracts; streaming + `sparsearray` arrive in Phase 5).

use std::collections::BTreeMap;
use std::path::Path;

use geo_types::{Coord, Geometry, LineString, Point, Polygon};
use osmpbf::{Element, ElementReader};

use super::SourceFeature;
use crate::collection::NodeLocations;
use crate::error::{Error, Result};
use crate::model::Value;

/// Planetiler-style feature id: `osm_id * 10 + source_type` (node=1, way=2).
const NODE_KIND: u64 = 1;
const WAY_KIND: u64 = 2;

/// Tag keys that make a *closed* way an area (polygon) rather than a line.
const AREA_KEYS: &[&str] = &[
    "building", "landuse", "natural", "leisure", "amenity", "boundary", "historic", "place",
    "shop", "tourism", "man_made", "water", "waterway", "aeroway", "craft", "office",
];

/// Read nodes and ways from an `.osm.pbf` file into [`SourceFeature`]s.
///
/// # Errors
/// Returns [`Error::Source`] if the PBF cannot be read.
pub fn read_osm(path: impl AsRef<Path>) -> Result<Vec<SourceFeature>> {
    let path = path.as_ref();
    let mut locations = NodeLocations::default();
    let mut features: Vec<SourceFeature> = Vec::new();

    // Pass 1: record all node locations; emit tagged nodes as points.
    reader(path)?
        .for_each(|element| match element {
            Element::Node(node) => {
                locations.put(node.id(), node.lon(), node.lat());
                let tags = collect_tags(node.tags());
                if !tags.is_empty() {
                    features.push(point(node.id(), node.lon(), node.lat(), tags));
                }
            }
            Element::DenseNode(node) => {
                locations.put(node.id(), node.lon(), node.lat());
                let tags = collect_tags(node.tags());
                if !tags.is_empty() {
                    features.push(point(node.id(), node.lon(), node.lat(), tags));
                }
            }
            Element::Way(_) | Element::Relation(_) => {}
        })
        .map_err(|_| Error::Source("failed to read OSM PBF (node pass)"))?;
    locations.finish();

    // Pass 2: assemble tagged ways from node locations.
    reader(path)?
        .for_each(|element| {
            if let Element::Way(way) = element {
                let tags = collect_tags(way.tags());
                if tags.is_empty() {
                    return;
                }
                let coords: Vec<(f64, f64)> =
                    way.refs().filter_map(|id| locations.get(id)).collect();
                if coords.len() < 2 {
                    return;
                }
                if let Some(feature) = way_feature(way.id(), &coords, tags) {
                    features.push(feature);
                }
            }
        })
        .map_err(|_| Error::Source("failed to read OSM PBF (way pass)"))?;

    Ok(features)
}

fn reader(path: &Path) -> Result<ElementReader<std::io::BufReader<std::fs::File>>> {
    ElementReader::from_path(path).map_err(|_| Error::Source("cannot open OSM PBF"))
}

fn point(id: i64, lon: f64, lat: f64, tags: BTreeMap<String, Value>) -> SourceFeature {
    SourceFeature {
        id: feature_id(id, NODE_KIND),
        geometry: Geometry::Point(Point::new(lon, lat)),
        tags,
    }
}

fn way_feature(
    id: i64,
    coords: &[(f64, f64)],
    tags: BTreeMap<String, Value>,
) -> Option<SourceFeature> {
    let closed = coords.len() >= 4 && coords.first() == coords.last();
    let ring: Vec<Coord<f64>> = coords.iter().map(|&(x, y)| Coord { x, y }).collect();
    let geometry = if closed && is_area(&tags) {
        Geometry::Polygon(Polygon::new(LineString(ring), vec![]))
    } else {
        Geometry::LineString(LineString(ring))
    };
    Some(SourceFeature {
        id: feature_id(id, WAY_KIND),
        geometry,
        tags,
    })
}

fn feature_id(osm_id: i64, kind: u64) -> u64 {
    osm_id.unsigned_abs() * 10 + kind
}

fn collect_tags<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> BTreeMap<String, Value> {
    tags.map(|(k, v)| (k.to_owned(), Value::String(v.to_owned())))
        .collect()
}

/// Whether a closed way's tags make it an area (polygon). Highways/barriers stay
/// lines even when closed (roundabouts, fences); `area=no` forces a line.
fn is_area(tags: &BTreeMap<String, Value>) -> bool {
    match tags.get("area").and_then(Value::as_str) {
        Some("no") => return false,
        Some("yes") => return true,
        _ => {}
    }
    if tags.contains_key("highway") || tags.contains_key("barrier") {
        return false;
    }
    AREA_KEYS.iter().any(|k| tags.contains_key(*k))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::from(*v)))
            .collect()
    }

    #[test]
    fn closed_building_is_polygon() {
        let square = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)];
        let f = way_feature(1, &square, tags(&[("building", "yes")])).unwrap();
        assert!(matches!(f.geometry, Geometry::Polygon(_)));
        assert_eq!(f.id, feature_id(1, WAY_KIND));
    }

    #[test]
    fn closed_highway_stays_line() {
        // A roundabout: closed but a line, not a polygon.
        let loop_ = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)];
        let f = way_feature(
            2,
            &loop_,
            tags(&[("highway", "residential"), ("junction", "roundabout")]),
        )
        .unwrap();
        assert!(matches!(f.geometry, Geometry::LineString(_)));
    }

    #[test]
    fn open_way_is_line() {
        let path = [(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)];
        let f = way_feature(3, &path, tags(&[("highway", "primary")])).unwrap();
        assert!(matches!(f.geometry, Geometry::LineString(_)));
    }

    #[test]
    fn area_no_forces_line() {
        let square = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)];
        let f = way_feature(4, &square, tags(&[("landuse", "grass"), ("area", "no")])).unwrap();
        assert!(matches!(f.geometry, Geometry::LineString(_)));
    }
}
