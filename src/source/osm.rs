//! OpenStreetMap `.osm.pbf` source reader (nodes, ways, relations).
//!
//! Two file passes plus a relation-assembly step: pass 1 records every node's
//! location (the compact in-memory [`NodeLocations`] store), emits tagged nodes
//! as point features, and collects multipolygon/route relation definitions;
//! pass 2 assembles ways into line/polygon geometry and captures the geometries
//! of ways that relations reference; then relations are assembled (multipolygon
//! ring-stitching + hole assignment, route multilinestrings). Features are
//! pushed to a sink as they are produced ([`stream_osm`]), so the pipeline never
//! holds them all. The node store is selectable via [`NodeStorage`] — in-RAM by
//! default, or an off-heap mmap store for planet scale; a compact `sparsearray`
//! variant is future work.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use geo_types::{
    Coord, Geometry, LineString, MultiLineString, MultiPolygon, Point, Polygon, coord,
};
use osmpbf::{Element, ElementReader, RelMemberType};

use super::{FeatureSink, NodeStorage, SourceFeature};
use crate::collection::{MmapNodeStore, NodeLocations, NodeStore, SparseNodeStore};
use crate::error::{Error, Result};
use crate::model::Value;

/// Planetiler-style feature id: `osm_id * 10 + source_type`.
const NODE_KIND: u64 = 1;
const WAY_KIND: u64 = 2;
const RELATION_KIND: u64 = 3;

/// Tag keys that make a *closed* way an area (polygon) rather than a line.
const AREA_KEYS: &[&str] = &[
    "building", "landuse", "natural", "leisure", "amenity", "boundary", "historic", "place",
    "shop", "tourism", "man_made", "water", "waterway", "aeroway", "craft", "office",
];

/// The relation types this reader assembles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RelationKind {
    /// `type=multipolygon` / `type=boundary` → polygon with holes.
    MultiPolygon,
    /// `type=route` → multilinestring.
    Route,
}

/// A relation collected in pass 1, assembled after pass 2.
struct PendingRelation {
    id: i64,
    kind: RelationKind,
    tags: BTreeMap<String, Value>,
    /// `(way_id, is_inner)` members, in order.
    members: Vec<(i64, bool)>,
}

/// Stream nodes, ways, and relations from an `.osm.pbf` into `sink`.
///
/// # Errors
/// Returns [`Error::Source`] if the PBF cannot be read.
pub fn stream_osm(path: &Path, sink: &mut FeatureSink, storage: NodeStorage) -> Result<()> {
    let mut locations: Box<dyn NodeStore> = match storage {
        NodeStorage::Ram => Box::new(NodeLocations::default()),
        NodeStorage::Mmap => Box::new(MmapNodeStore::new()?),
        NodeStorage::Sparse => Box::new(SparseNodeStore::new()?),
    };
    let mut relations: Vec<PendingRelation> = Vec::new();
    let mut needed_ways: HashSet<i64> = HashSet::new();
    // osmpbf's `for_each` closure can't return a Result, so capture the first
    // sink error and skip the rest, then propagate it after the pass.
    let mut sink_err: Option<Error> = None;

    // Pass 1: node locations + point features + relation definitions.
    let pass1_start = std::time::Instant::now();
    reader(path)?
        .for_each(|element| match element {
            Element::Node(node) => {
                locations.put(node.id(), node.lon(), node.lat());
                let info = node.info();
                emit_node(
                    node.id(),
                    node.lon(),
                    node.lat(),
                    collect_tags(node.tags()),
                    info.version(),
                    info.milli_timestamp(),
                    sink,
                    &mut sink_err,
                );
            }
            Element::DenseNode(node) => {
                locations.put(node.id(), node.lon(), node.lat());
                emit_node(
                    node.id(),
                    node.lon(),
                    node.lat(),
                    collect_tags(node.tags()),
                    node.info().map(|i| i.version()),
                    node.info().map(|i| i.milli_timestamp()),
                    sink,
                    &mut sink_err,
                );
            }
            Element::Relation(relation) => {
                let mut tags = collect_tags(relation.tags());
                if let Some(kind) = relation_kind(&tags) {
                    let members: Vec<(i64, bool)> = relation
                        .members()
                        .filter(|m| m.member_type == RelMemberType::Way)
                        .map(|m| (m.member_id, m.role().unwrap_or("") == "inner"))
                        .collect();
                    needed_ways.extend(members.iter().map(|&(id, _)| id));
                    let info = relation.info();
                    add_metadata(
                        &mut tags,
                        relation.id(),
                        "relation",
                        info.version(),
                        info.milli_timestamp(),
                    );
                    relations.push(PendingRelation {
                        id: relation.id(),
                        kind,
                        tags,
                        members,
                    });
                }
            }
            Element::Way(_) => {}
        })
        .map_err(|_| Error::Source("failed to read OSM PBF (node pass)"))?;
    if let Some(err) = sink_err.take() {
        return Err(err);
    }
    locations.finish()?;
    tracing::info!(
        elapsed_s = pass1_start.elapsed().as_secs_f64(),
        "osm pass 1 (nodes) done"
    );

    // Pass 2: way features + capture member-way geometries for relations.
    let pass2_start = std::time::Instant::now();
    let mut member_ways: HashMap<i64, Vec<(f64, f64)>> = HashMap::new();
    reader(path)?
        .for_each(|element| {
            if sink_err.is_some() {
                return;
            }
            if let Element::Way(way) = element {
                let coords: Vec<(f64, f64)> =
                    way.refs().filter_map(|id| locations.get(id)).collect();
                if needed_ways.contains(&way.id()) {
                    member_ways.insert(way.id(), coords.clone());
                }
                let mut tags = collect_tags(way.tags());
                if tags.is_empty() || coords.len() < 2 {
                    return;
                }
                let info = way.info();
                add_metadata(
                    &mut tags,
                    way.id(),
                    "way",
                    info.version(),
                    info.milli_timestamp(),
                );
                if let Some(feature) = way_feature(way.id(), &coords, tags)
                    && let Err(err) = sink(feature)
                {
                    sink_err = Some(err);
                }
            }
        })
        .map_err(|_| Error::Source("failed to read OSM PBF (way pass)"))?;
    if let Some(err) = sink_err.take() {
        return Err(err);
    }

    tracing::info!(
        elapsed_s = pass2_start.elapsed().as_secs_f64(),
        "osm pass 2 (ways) done"
    );

    // Assemble relations from their (now known) member geometries.
    for relation in relations {
        if let Some(feature) = assemble_relation(&relation, &member_ways) {
            sink(feature)?;
        }
    }
    Ok(())
}

/// Collect all OSM features into a `Vec` (small extracts / tests). Prefer
/// [`stream_osm`] for large inputs.
pub fn read_osm(path: impl AsRef<Path>) -> Result<Vec<SourceFeature>> {
    let mut features = Vec::new();
    stream_osm(
        path.as_ref(),
        &mut |feature| {
            features.push(feature);
            Ok(())
        },
        NodeStorage::Ram,
    )?;
    Ok(features)
}

/// Read the `.osm.pbf` header's bounding box (lon/lat `west, south, east,
/// north`), if present — the natural default tile bounds, matching Planetiler.
pub fn osm_bounds(path: &Path) -> Result<Option<(f64, f64, f64, f64)>> {
    let mut reader =
        osmpbf::BlobReader::from_path(path).map_err(|_| Error::Source("cannot open OSM PBF"))?;
    let Some(blob) = reader.next() else {
        return Ok(None);
    };
    let blob = blob.map_err(|_| Error::Source("cannot read OSM PBF header"))?;
    let Ok(header) = blob.to_headerblock() else {
        return Ok(None);
    };
    Ok(header.bbox().map(|b| (b.left, b.bottom, b.right, b.top)))
}

#[allow(clippy::too_many_arguments)]
fn emit_node(
    id: i64,
    lon: f64,
    lat: f64,
    mut tags: BTreeMap<String, Value>,
    version: Option<i32>,
    timestamp_millis: Option<i64>,
    sink: &mut FeatureSink,
    sink_err: &mut Option<Error>,
) {
    if tags.is_empty() || sink_err.is_some() {
        return;
    }
    add_metadata(&mut tags, id, "node", version, timestamp_millis);
    if let Err(err) = sink(point(id, lon, lat, tags)) {
        *sink_err = Some(err);
    }
}

/// Add the `osm-qa`-style element-metadata attributes (`@id`, `@type`,
/// `@version`, `@timestamp` in epoch seconds) that Planetiler's osm-qa profile
/// attaches to every feature.
fn add_metadata(
    tags: &mut BTreeMap<String, Value>,
    id: i64,
    element_type: &str,
    version: Option<i32>,
    timestamp_millis: Option<i64>,
) {
    tags.insert("@id".to_owned(), Value::Int(id));
    tags.insert("@type".to_owned(), Value::String(element_type.to_owned()));
    if let Some(version) = version {
        tags.insert("@version".to_owned(), Value::Int(i64::from(version)));
    }
    if let Some(millis) = timestamp_millis {
        tags.insert("@timestamp".to_owned(), Value::Int(millis / 1000));
    }
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
    let geometry = if closed && is_area(&tags) {
        Geometry::Polygon(Polygon::new(line_string(coords), vec![]))
    } else {
        Geometry::LineString(line_string(coords))
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

// --- relations ---------------------------------------------------------------

fn relation_kind(tags: &BTreeMap<String, Value>) -> Option<RelationKind> {
    match tags.get("type").and_then(Value::as_str) {
        Some("multipolygon" | "boundary") => Some(RelationKind::MultiPolygon),
        Some("route") => Some(RelationKind::Route),
        _ => None,
    }
}

fn assemble_relation(
    relation: &PendingRelation,
    member_ways: &HashMap<i64, Vec<(f64, f64)>>,
) -> Option<SourceFeature> {
    let geometry = match relation.kind {
        RelationKind::MultiPolygon => assemble_multipolygon(relation, member_ways)?,
        RelationKind::Route => assemble_route(relation, member_ways)?,
    };
    Some(SourceFeature {
        id: feature_id(relation.id, RELATION_KIND),
        geometry,
        tags: relation.tags.clone(),
    })
}

fn assemble_multipolygon(
    relation: &PendingRelation,
    member_ways: &HashMap<i64, Vec<(f64, f64)>>,
) -> Option<Geometry<f64>> {
    let mut outer_fragments = Vec::new();
    let mut inner_fragments = Vec::new();
    for &(way_id, is_inner) in &relation.members {
        if let Some(coords) = member_ways.get(&way_id) {
            if is_inner {
                inner_fragments.push(coords.clone());
            } else {
                outer_fragments.push(coords.clone());
            }
        }
    }

    let outer_rings = assemble_rings(outer_fragments);
    let inner_rings = assemble_rings(inner_fragments);
    if outer_rings.is_empty() {
        return None;
    }

    let polygons: Vec<Polygon<f64>> = outer_rings
        .iter()
        .map(|outer| {
            let holes: Vec<LineString<f64>> = inner_rings
                .iter()
                .filter(|inner| ring_contains(outer, inner[0]))
                .map(|inner| line_string(inner))
                .collect();
            Polygon::new(line_string(outer), holes)
        })
        .collect();

    Some(if polygons.len() == 1 {
        Geometry::Polygon(polygons.into_iter().next().unwrap())
    } else {
        Geometry::MultiPolygon(MultiPolygon(polygons))
    })
}

fn assemble_route(
    relation: &PendingRelation,
    member_ways: &HashMap<i64, Vec<(f64, f64)>>,
) -> Option<Geometry<f64>> {
    let lines: Vec<LineString<f64>> = relation
        .members
        .iter()
        .filter_map(|&(way_id, _)| member_ways.get(&way_id))
        .filter(|coords| coords.len() >= 2)
        .map(|coords| line_string(coords))
        .collect();
    (!lines.is_empty()).then_some(Geometry::MultiLineString(MultiLineString(lines)))
}

/// Stitch way fragments into closed rings by connecting matching endpoints
/// (member ways share exact node coordinates, so equality matching is safe).
///
/// O(total fragments): both endpoints of every fragment are indexed in a
/// hash map up front (hashing the exact f64 bit patterns — all coordinates come
/// from the same node store, so equal points are bit-identical), and each
/// extension is a lookup instead of a scan. Large admin boundaries have
/// thousands of member ways, where the old linear scan was quadratic.
fn assemble_rings(fragments: Vec<Vec<(f64, f64)>>) -> Vec<Vec<(f64, f64)>> {
    fn key((x, y): (f64, f64)) -> (u64, u64) {
        (x.to_bits(), y.to_bits())
    }

    // Fragment slots are taken (`None`) as they are consumed; the endpoint index
    // is cleaned up lazily by popping consumed entries when encountered.
    let mut slots: Vec<Option<Vec<(f64, f64)>>> = fragments
        .into_iter()
        .filter(|f| f.len() >= 2)
        .map(Some)
        .collect();
    let mut by_endpoint: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
    for (index, fragment) in slots.iter().enumerate() {
        let fragment = fragment.as_ref().unwrap();
        by_endpoint.entry(key(fragment[0])).or_default().push(index);
        by_endpoint
            .entry(key(*fragment.last().unwrap()))
            .or_default()
            .push(index);
    }

    let mut rings = Vec::new();
    for start in 0..slots.len() {
        let Some(mut ring) = slots[start].take() else {
            continue;
        };
        loop {
            if is_closed(&ring) {
                break;
            }
            let end_key = key(*ring.last().unwrap());
            let next_index = match by_endpoint.get_mut(&end_key) {
                Some(candidates) => loop {
                    match candidates.last() {
                        Some(&candidate) if slots[candidate].is_none() => {
                            candidates.pop(); // consumed earlier; drop lazily
                        }
                        Some(&candidate) => break Some(candidate),
                        None => break None,
                    }
                },
                None => None,
            };
            let Some(index) = next_index else {
                break; // cannot extend or close this ring
            };
            let next = slots[index].take().unwrap();
            if key(next[0]) == end_key {
                ring.extend_from_slice(&next[1..]);
            } else {
                ring.extend(next.iter().rev().skip(1).copied());
            }
        }
        if is_closed(&ring) {
            rings.push(ring);
        }
    }
    rings
}

fn is_closed(ring: &[(f64, f64)]) -> bool {
    ring.len() >= 4 && ring.first() == ring.last()
}

/// Ray-casting point-in-polygon against a (closed) ring.
fn ring_contains(ring: &[(f64, f64)], (px, py): (f64, f64)) -> bool {
    let mut inside = false;
    let n = ring.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];
        if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn line_string(coords: &[(f64, f64)]) -> LineString<f64> {
    LineString(
        coords
            .iter()
            .map(|&(x, y)| coord! { x: x, y: y })
            .collect::<Vec<Coord<f64>>>(),
    )
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

    #[test]
    fn stitches_two_open_fragments_into_a_ring() {
        // Two halves of a square that share endpoints.
        let a = vec![(0.0, 0.0), (2.0, 0.0), (2.0, 2.0)];
        let b = vec![(2.0, 2.0), (0.0, 2.0), (0.0, 0.0)];
        let rings = assemble_rings(vec![a, b]);
        assert_eq!(rings.len(), 1);
        assert!(is_closed(&rings[0]));
    }

    #[test]
    fn multipolygon_assigns_hole_to_outer() {
        let outer = vec![
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ];
        let inner = vec![(3.0, 3.0), (6.0, 3.0), (6.0, 6.0), (3.0, 6.0), (3.0, 3.0)];
        let mut member_ways = HashMap::new();
        member_ways.insert(1, outer);
        member_ways.insert(2, inner);
        let relation = PendingRelation {
            id: 7,
            kind: RelationKind::MultiPolygon,
            tags: tags(&[("type", "multipolygon"), ("natural", "water")]),
            members: vec![(1, false), (2, true)],
        };
        let feature = assemble_relation(&relation, &member_ways).unwrap();
        assert_eq!(feature.id, feature_id(7, RELATION_KIND));
        match feature.geometry {
            Geometry::Polygon(p) => assert_eq!(p.interiors().len(), 1, "the inner ring is a hole"),
            other => panic!("expected polygon, got {other:?}"),
        }
    }

    #[test]
    fn route_relation_is_multilinestring() {
        let mut member_ways = HashMap::new();
        member_ways.insert(1, vec![(0.0, 0.0), (1.0, 1.0)]);
        member_ways.insert(2, vec![(1.0, 1.0), (2.0, 2.0)]);
        let relation = PendingRelation {
            id: 8,
            kind: RelationKind::Route,
            tags: tags(&[("type", "route"), ("route", "bicycle")]),
            members: vec![(1, false), (2, false)],
        };
        let feature = assemble_relation(&relation, &member_ways).unwrap();
        match feature.geometry {
            Geometry::MultiLineString(mls) => assert_eq!(mls.0.len(), 2),
            other => panic!("expected multilinestring, got {other:?}"),
        }
    }
}
