//! OpenStreetMap `.osm.pbf` source reader (nodes, ways, relations).
//!
//! Two file passes plus a relation-assembly step: pass 1 records every node's
//! location (a [`NodeStore`] selected via [`NodeStorage`]), emits tagged nodes
//! as point features, and collects multipolygon/route relation definitions;
//! pass 2 assembles ways into line/polygon geometry (sharing the finished node
//! store across workers) and streams the geometries of relation member ways to
//! a file-backed [`WayGeometryStore`]; then relations are assembled
//! (multipolygon ring-stitching + hole assignment, route multilinestrings).
//! The between-pass state is deliberately compact — packed on-disk member
//! geometries, a sorted id slice, postcard-encoded pending tags — because on a
//! continent extract it otherwise dominates peak memory. Each pass reads blobs sequentially but
//! decodes them in parallel windows with ordered consumption
//! ([`for_each_block_parallel`]), so node ids stay ascending and feature
//! emission stays deterministic while the expensive protobuf/zlib decode uses
//! every core. Features are pushed to a sink as they are produced
//! ([`stream_osm`]), so the pipeline never holds them all.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;

use geo::CoordsIter;
use geo_types::{
    Coord, Geometry, LineString, MultiLineString, MultiPolygon, Point, Polygon, coord,
};
use osmpbf::{Blob, BlobDecode, BlobReader, PrimitiveBlock, RelMemberType};
use rayon::prelude::*;

use std::sync::Arc;

use super::{FeatureSink, NodeStorage, RelationMember, RelationPreprocessor, SourceFeature};
use crate::collection::{
    MmapNodeStore, NodeLocations, NodeStore, SparseNodeStore, WayGeometryStore,
};
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
///
/// Tags are held postcard-encoded rather than as a live `BTreeMap`: a
/// continent extract collects millions of pending relations, and the packed
/// blob is several times smaller than the tree it decodes back into at
/// assembly time.
struct PendingRelation {
    id: i64,
    kind: RelationKind,
    /// postcard-encoded `BTreeMap<String, Value>`.
    tags: Vec<u8>,
    /// `(way_id, is_inner)` members, in order.
    members: Vec<(i64, bool)>,
}

impl PendingRelation {
    fn encode_tags(tags: &BTreeMap<String, Value>) -> Vec<u8> {
        postcard::to_allocvec(tags).expect("relation tag encode is infallible")
    }

    fn decode_tags(&self) -> BTreeMap<String, Value> {
        postcard::from_bytes(&self.tags).expect("tags were encoded by pass 1")
    }
}

/// Stream nodes, ways, and relations from an `.osm.pbf` into `sink`.
///
/// # Errors
/// Returns [`Error::Source`] if the PBF cannot be read.
pub fn stream_osm(
    path: &Path,
    sink: &mut FeatureSink,
    storage: NodeStorage,
    preprocess: Option<&RelationPreprocessor>,
) -> Result<()> {
    let mut locations: Box<dyn NodeStore> = match storage {
        NodeStorage::Ram => Box::new(NodeLocations::default()),
        NodeStorage::Mmap => Box::new(MmapNodeStore::new()?),
        NodeStorage::Sparse => Box::new(SparseNodeStore::new()?),
    };
    let mut relations: Vec<PendingRelation> = Vec::new();
    // Way ids some relation references; sorted + deduped between the passes so
    // pass 2 can binary-search a flat slice (a HashSet of tens of millions of
    // ids costs several times the 8 bytes/id this does).
    let mut needed_ways: Vec<i64> = Vec::new();
    // way id -> memberships the profile asked to index (via `preprocess`).
    let mut way_relations: HashMap<i64, Vec<RelationMember>> = HashMap::new();

    // Pass 1: node locations + point features + relation definitions.
    let pass1_start = std::time::Instant::now();
    for_each_block_parallel(
        path,
        |block| {
            let mut out = NodeBlock::default();
            for group in block.groups() {
                for node in group.nodes() {
                    out.locations.push((node.id(), node.lon(), node.lat()));
                    let tags = collect_tags(node.tags());
                    if !tags.is_empty() {
                        let info = node.info();
                        out.features.push(node_feature(
                            node.id(),
                            node.lon(),
                            node.lat(),
                            tags,
                            info.version(),
                            info.milli_timestamp(),
                        ));
                    }
                }
                for node in group.dense_nodes() {
                    out.locations.push((node.id(), node.lon(), node.lat()));
                    let tags = collect_tags(node.tags());
                    if !tags.is_empty() {
                        out.features.push(node_feature(
                            node.id(),
                            node.lon(),
                            node.lat(),
                            tags,
                            node.info().map(|i| i.version()),
                            node.info().map(|i| i.milli_timestamp()),
                        ));
                    }
                }
                for relation in group.relations() {
                    let mut tags = collect_tags(relation.tags());
                    if let Some(preprocess) = preprocess
                        && let Some(info) = preprocess(relation.id(), &tags)
                    {
                        let members: Vec<(i64, String)> = relation
                            .members()
                            .filter(|m| m.member_type == RelMemberType::Way)
                            .map(|m| (m.member_id, m.role().unwrap_or("").to_owned()))
                            .collect();
                        out.relation_infos.push((relation.id(), info, members));
                    }
                    let Some(kind) = relation_kind(&tags) else {
                        continue;
                    };
                    let members: Vec<(i64, bool)> = relation
                        .members()
                        .filter(|m| m.member_type == RelMemberType::Way)
                        .map(|m| (m.member_id, m.role().unwrap_or("") == "inner"))
                        .collect();
                    let info = relation.info();
                    add_metadata(
                        &mut tags,
                        relation.id(),
                        "relation",
                        info.version(),
                        info.milli_timestamp(),
                    );
                    out.relations.push(PendingRelation {
                        id: relation.id(),
                        kind,
                        tags: PendingRelation::encode_tags(&tags),
                        members,
                    });
                }
            }
            out
        },
        |out| {
            for (id, lon, lat) in out.locations {
                locations.put(id, lon, lat);
            }
            for feature in out.features {
                sink(feature)?;
            }
            for relation in out.relations {
                needed_ways.extend(relation.members.iter().map(|&(id, _)| id));
                relations.push(relation);
            }
            for (relation_id, info, members) in out.relation_infos {
                let info = Arc::new(info);
                for (way_id, role) in members {
                    way_relations
                        .entry(way_id)
                        .or_default()
                        .push(RelationMember {
                            role,
                            relation_id,
                            info: Arc::clone(&info),
                        });
                }
            }
            Ok(())
        },
    )?;
    locations.finish()?;
    needed_ways.sort_unstable();
    needed_ways.dedup();
    tracing::info!(
        elapsed_s = pass1_start.elapsed().as_secs_f64(),
        "osm pass 1 (nodes) done"
    );

    // Pass 2: way features + capture member-way geometries for relations. The
    // finished node store is shared read-only across the decode workers, so the
    // per-node location lookups parallelize along with the decode. Member-way
    // geometries stream to the file-backed store, not the heap.
    let pass2_start = std::time::Instant::now();
    let mut member_ways = WayGeometryStore::new()?;
    {
        let store: &dyn NodeStore = &*locations;
        let needed = &needed_ways;
        let memberships = &way_relations;
        for_each_block_parallel(
            path,
            |block| {
                let mut out = WayBlock::default();
                for group in block.groups() {
                    for way in group.ways() {
                        let coords: Vec<(f64, f64)> =
                            way.refs().filter_map(|id| store.get(id)).collect();
                        if needed.binary_search(&way.id()).is_ok() {
                            out.member_ways.push((way.id(), coords.clone()));
                        }
                        let mut tags = collect_tags(way.tags());
                        if tags.is_empty() || coords.len() < 2 {
                            continue;
                        }
                        let info = way.info();
                        add_metadata(
                            &mut tags,
                            way.id(),
                            "way",
                            info.version(),
                            info.milli_timestamp(),
                        );
                        let relations = memberships.get(&way.id()).cloned().unwrap_or_default();
                        if let Some(feature) = way_feature(way.id(), &coords, tags, relations) {
                            out.features.push(feature);
                        }
                    }
                }
                out
            },
            |out| {
                for (way_id, coords) in out.member_ways {
                    member_ways.put(way_id, &coords);
                }
                for feature in out.features {
                    sink(feature)?;
                }
                Ok(())
            },
        )?;
    }
    member_ways.finish()?;
    tracing::info!(
        elapsed_s = pass2_start.elapsed().as_secs_f64(),
        "osm pass 2 (ways) done"
    );

    // Assemble relations from their (now known) member geometries. A
    // planet-scale relation (ocean coastline, ice-shelf multipolygon) can carry
    // millions of coords and, once rendered, clip into the unbounded batch that
    // walls the write stage — so name the big ones by id as they pass here,
    // upstream of any batching (H-A probe). `coords` is the driver: it sets the
    // number of tile pieces the render expands into. `members` is only a
    // structural-outlier flag — routine bus/route networks carry thousands of
    // members with trivial geometry, so its bar is high to keep the log honest.
    const RELATION_COORDS_WARN: usize = 1_000_000;
    const RELATION_MEMBERS_WARN: usize = 50_000;
    for relation in relations {
        if let Some(feature) = assemble_relation(&relation, &member_ways) {
            let members = relation.members.len();
            let coords = feature.geometry.coords_count();
            if members > RELATION_MEMBERS_WARN || coords > RELATION_COORDS_WARN {
                let kind = match relation.kind {
                    RelationKind::MultiPolygon => "multipolygon",
                    RelationKind::Route => "route",
                };
                tracing::info!(
                    relation_id = relation.id,
                    kind,
                    members,
                    coords,
                    "large relation assembled"
                );
            }
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
        None,
    )?;
    Ok(features)
}

/// Read the `.osm.pbf` header's bounding box (lon/lat `west, south, east,
/// north`), if present — the natural default tile bounds, matching Planetiler.
pub fn osm_bounds(path: &Path) -> Result<Option<(f64, f64, f64, f64)>> {
    let mut reader = osmpbf::BlobReader::from_path(path).map_err(|e| Error::SourceDetail {
        context: "cannot open OSM PBF",
        message: e.to_string(),
    })?;
    let Some(blob) = reader.next() else {
        return Ok(None);
    };
    let blob = blob.map_err(|e| Error::SourceDetail {
        context: "cannot read OSM PBF header",
        message: e.to_string(),
    })?;
    let Ok(header) = blob.to_headerblock() else {
        return Ok(None);
    };
    Ok(header.bbox().map(|b| (b.left, b.bottom, b.right, b.top)))
}

/// Decoded output of one pass-1 blob, in element order.
/// One relation the profile's preprocessor asked to index:
/// `(relation_id, profile info, members as (way_id, role))`.
type IndexedRelation = (i64, BTreeMap<String, Value>, Vec<(i64, String)>);

#[derive(Default)]
struct NodeBlock {
    locations: Vec<(i64, f64, f64)>,
    features: Vec<SourceFeature>,
    relations: Vec<PendingRelation>,
    relation_infos: Vec<IndexedRelation>,
}

/// Decoded output of one pass-2 blob, in element order.
#[derive(Default)]
struct WayBlock {
    member_ways: Vec<(i64, Vec<(f64, f64)>)>,
    features: Vec<SourceFeature>,
}

/// Read the file's blobs sequentially, decode windows of them across cores, and
/// hand each block's extracted output to `consume` in file order. The heavy
/// work (zlib + protobuf decode, tag/geometry extraction) runs in `decode` on
/// rayon workers; `consume` stays serial and ordered, so node ids reach the
/// store ascending and feature emission is deterministic. Memory is bounded by
/// one window of decoded output.
fn for_each_block_parallel<T, D, C>(path: &Path, decode: D, mut consume: C) -> Result<()>
where
    T: Send,
    D: Fn(&PrimitiveBlock) -> T + Sync,
    C: FnMut(T) -> Result<()>,
{
    const WINDOW: usize = 64;
    let mut reader = BlobReader::from_path(path).map_err(|e| Error::SourceDetail {
        context: "cannot open OSM PBF",
        message: e.to_string(),
    })?;
    loop {
        let window: Vec<Blob> = reader
            .by_ref()
            .take(WINDOW)
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| Error::SourceDetail {
                context: "failed to read OSM PBF",
                message: e.to_string(),
            })?;
        if window.is_empty() {
            break;
        }
        let outputs: Vec<Option<T>> = window
            .par_iter()
            .map(|blob| match blob.decode() {
                Ok(BlobDecode::OsmData(block)) => Ok(Some(decode(&block))),
                Ok(_) => Ok(None),
                Err(e) => Err(Error::SourceDetail {
                    context: "failed to decode OSM PBF block",
                    message: e.to_string(),
                }),
            })
            .collect::<Result<_>>()?;
        for output in outputs.into_iter().flatten() {
            consume(output)?;
        }
    }
    Ok(())
}

/// Build a tagged node's point feature with the osm-qa metadata attached.
fn node_feature(
    id: i64,
    lon: f64,
    lat: f64,
    mut tags: BTreeMap<String, Value>,
    version: Option<i32>,
    timestamp_millis: Option<i64>,
) -> SourceFeature {
    add_metadata(&mut tags, id, "node", version, timestamp_millis);
    point(id, lon, lat, tags)
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

fn point(id: i64, lon: f64, lat: f64, tags: BTreeMap<String, Value>) -> SourceFeature {
    SourceFeature {
        id: feature_id(id, NODE_KIND),
        geometry: Geometry::Point(Point::new(lon, lat)),
        tags,
        relations: Vec::new(),
    }
}

fn way_feature(
    id: i64,
    coords: &[(f64, f64)],
    tags: BTreeMap<String, Value>,
    relations: Vec<RelationMember>,
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
        relations,
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
    member_ways: &WayGeometryStore,
) -> Option<SourceFeature> {
    let geometry = match relation.kind {
        RelationKind::MultiPolygon => assemble_multipolygon(relation, member_ways)?,
        RelationKind::Route => assemble_route(relation, member_ways)?,
    };
    Some(SourceFeature {
        id: feature_id(relation.id, RELATION_KIND),
        geometry,
        tags: relation.decode_tags(),
        relations: Vec::new(),
    })
}

fn assemble_multipolygon(
    relation: &PendingRelation,
    member_ways: &WayGeometryStore,
) -> Option<Geometry<f64>> {
    let mut outer_fragments = Vec::new();
    let mut inner_fragments = Vec::new();
    for &(way_id, is_inner) in &relation.members {
        if let Some(coords) = member_ways.get(way_id) {
            if is_inner {
                inner_fragments.push(coords);
            } else {
                outer_fragments.push(coords);
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
    member_ways: &WayGeometryStore,
) -> Option<Geometry<f64>> {
    let lines: Vec<LineString<f64>> = relation
        .members
        .iter()
        .filter_map(|&(way_id, _)| member_ways.get(way_id))
        .filter(|coords| coords.len() >= 2)
        .map(|coords| line_string(&coords))
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
    // Candidates stay in fragment order and are consumed from the front, so an
    // ambiguous junction (>2 fragments sharing an endpoint) picks the same
    // fragment the old first-match linear scan did — output is unchanged.
    let mut by_endpoint: HashMap<(u64, u64), VecDeque<usize>> = HashMap::new();
    for (index, fragment) in slots.iter().enumerate() {
        let fragment = fragment.as_ref().unwrap();
        by_endpoint
            .entry(key(fragment[0]))
            .or_default()
            .push_back(index);
        by_endpoint
            .entry(key(*fragment.last().unwrap()))
            .or_default()
            .push_back(index);
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
                    match candidates.front() {
                        Some(&candidate) if slots[candidate].is_none() => {
                            candidates.pop_front(); // consumed earlier; drop lazily
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
        let f = way_feature(1, &square, tags(&[("building", "yes")]), Vec::new()).unwrap();
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
            Vec::new(),
        )
        .unwrap();
        assert!(matches!(f.geometry, Geometry::LineString(_)));
    }

    #[test]
    fn open_way_is_line() {
        let path = [(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)];
        let f = way_feature(3, &path, tags(&[("highway", "primary")]), Vec::new()).unwrap();
        assert!(matches!(f.geometry, Geometry::LineString(_)));
    }

    #[test]
    fn area_no_forces_line() {
        let square = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)];
        let f = way_feature(
            4,
            &square,
            tags(&[("landuse", "grass"), ("area", "no")]),
            Vec::new(),
        )
        .unwrap();
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
        let mut member_ways = WayGeometryStore::new().unwrap();
        member_ways.put(1, &outer);
        member_ways.put(2, &inner);
        member_ways.finish().unwrap();
        let relation = PendingRelation {
            id: 7,
            kind: RelationKind::MultiPolygon,
            tags: PendingRelation::encode_tags(&tags(&[
                ("type", "multipolygon"),
                ("natural", "water"),
            ])),
            members: vec![(1, false), (2, true)],
        };
        let feature = assemble_relation(&relation, &member_ways).unwrap();
        assert_eq!(feature.id, feature_id(7, RELATION_KIND));
        assert_eq!(feature.tags.get("natural"), Some(&Value::from("water")));
        match feature.geometry {
            Geometry::Polygon(p) => assert_eq!(p.interiors().len(), 1, "the inner ring is a hole"),
            other => panic!("expected polygon, got {other:?}"),
        }
    }

    #[test]
    fn route_relation_is_multilinestring() {
        let mut member_ways = WayGeometryStore::new().unwrap();
        member_ways.put(1, &[(0.0, 0.0), (1.0, 1.0)]);
        member_ways.put(2, &[(1.0, 1.0), (2.0, 2.0)]);
        member_ways.finish().unwrap();
        let relation = PendingRelation {
            id: 8,
            kind: RelationKind::Route,
            tags: PendingRelation::encode_tags(&tags(&[("type", "route"), ("route", "bicycle")])),
            members: vec![(1, false), (2, false)],
        };
        let feature = assemble_relation(&relation, &member_ways).unwrap();
        match feature.geometry {
            Geometry::MultiLineString(mls) => assert_eq!(mls.0.len(), 2),
            other => panic!("expected multilinestring, got {other:?}"),
        }
    }
}
