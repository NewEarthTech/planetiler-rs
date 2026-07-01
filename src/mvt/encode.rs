//! MVT tile encoding.
//!
//! Turns the structured [`Tile`] representation into MVT protobuf bytes,
//! matching upstream Planetiler's observable choices for parity:
//!
//! - layers emitted **sorted by name** (empty layers skipped);
//! - per-layer `keys`/`values` dictionaries **deduplicated in first-seen
//!   order**;
//! - attribute value mapping `String→string`, `Int→sint`, `Uint→uint`,
//!   `Float→float`, `Double→double`, `Bool→bool` (upstream carries integers as
//!   `sint` and floating point as `double` in the standard pipeline);
//! - feature order is preserved as given (the pipeline feeds features in
//!   `SortKey` order).
//!
//! Per-feature tag order is deterministic (attributes are a sorted `BTreeMap`)
//! rather than replicating Java's `HashMap` iteration order, so encoded bytes
//! are reproducible and semantically identical to upstream, though not
//! byte-identical. See the Phase 1 parity notes.

use std::collections::HashMap;

use prost::Message;

use super::geometry;
use super::proto;
use super::proto::tile::GeomType;
use super::{Geometry, Layer, Tile};
use crate::model::Value;

/// Encode a structured [`Tile`] into MVT protobuf bytes (uncompressed).
///
/// Layers with no features are skipped and the rest are ordered by name.
pub fn encode_tile(tile: &Tile) -> Vec<u8> {
    let mut layers: Vec<&Layer> = tile
        .layers
        .iter()
        .filter(|l| !l.features.is_empty())
        .collect();
    layers.sort_by(|a, b| a.name.cmp(&b.name));
    let proto_layers = layers.into_iter().map(encode_layer).collect();
    proto::Tile {
        layers: proto_layers,
    }
    .encode_to_vec()
}

fn encode_layer(layer: &Layer) -> proto::tile::Layer {
    let mut dict = Dictionary::default();
    let mut features = Vec::with_capacity(layer.features.len());
    for feature in &layer.features {
        let (geom_type, geometry) = encode_geometry(&feature.geometry);
        let mut tags = Vec::with_capacity(feature.attributes.len() * 2);
        for (key, value) in &feature.attributes {
            tags.push(dict.intern_key(key));
            tags.push(dict.intern_value(value));
        }
        features.push(proto::tile::Feature {
            id: (feature.id != 0).then_some(feature.id),
            tags,
            r#type: Some(geom_type as i32),
            geometry,
        });
    }
    proto::tile::Layer {
        version: layer.version,
        name: layer.name.clone(),
        features,
        keys: dict.keys,
        values: dict.values,
        extent: Some(layer.extent),
    }
}

fn encode_geometry(geometry: &Geometry) -> (GeomType, Vec<u32>) {
    match geometry {
        Geometry::Points(points) => (GeomType::Point, geometry::encode_points(points)),
        Geometry::Lines(lines) => (GeomType::Linestring, geometry::encode_lines(lines)),
        Geometry::Polygons(rings) => (GeomType::Polygon, geometry::encode_polygons(rings)),
        Geometry::Unknown => (GeomType::Unknown, Vec::new()),
    }
}

/// First-seen-order key/value interning for one layer.
#[derive(Default)]
struct Dictionary {
    keys: Vec<String>,
    key_index: HashMap<String, u32>,
    values: Vec<proto::tile::Value>,
    value_index: HashMap<ValueKey, u32>,
}

impl Dictionary {
    fn intern_key(&mut self, key: &str) -> u32 {
        if let Some(&i) = self.key_index.get(key) {
            return i;
        }
        let i = self.keys.len() as u32;
        self.keys.push(key.to_owned());
        self.key_index.insert(key.to_owned(), i);
        i
    }

    fn intern_value(&mut self, value: &Value) -> u32 {
        let vk = ValueKey::from(value);
        if let Some(&i) = self.value_index.get(&vk) {
            return i;
        }
        let i = self.values.len() as u32;
        self.values.push(to_proto_value(value));
        self.value_index.insert(vk, i);
        i
    }
}

/// A hashable/equatable projection of [`Value`] for dictionary dedup. Floats
/// dedup by bit pattern, and distinct variants never collide (so `Int(5)` and
/// `Double(5.0)` are separate entries, matching upstream's dedup-by-equals).
#[derive(Hash, PartialEq, Eq)]
enum ValueKey {
    String(String),
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(u32),
    Double(u64),
}

impl From<&Value> for ValueKey {
    fn from(value: &Value) -> Self {
        match value {
            Value::String(s) => ValueKey::String(s.clone()),
            Value::Bool(b) => ValueKey::Bool(*b),
            Value::Int(i) => ValueKey::Int(*i),
            Value::Uint(u) => ValueKey::Uint(*u),
            Value::Float(f) => ValueKey::Float(f.to_bits()),
            Value::Double(d) => ValueKey::Double(d.to_bits()),
        }
    }
}

fn to_proto_value(value: &Value) -> proto::tile::Value {
    let mut pv = proto::tile::Value::default();
    match value {
        Value::String(s) => pv.string_value = Some(s.clone()),
        Value::Bool(b) => pv.bool_value = Some(*b),
        Value::Int(i) => pv.sint_value = Some(*i),
        Value::Uint(u) => pv.uint_value = Some(*u),
        Value::Float(f) => pv.float_value = Some(*f),
        Value::Double(d) => pv.double_value = Some(*d),
    }
    pv
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::{Feature, decode_tile};
    use super::*;

    fn attrs(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn line_feature(id: u64, pairs: &[(&str, Value)]) -> Feature {
        Feature {
            id,
            geometry: Geometry::Lines(vec![vec![(0, 0), (10, 10)]]),
            attributes: attrs(pairs),
        }
    }

    #[test]
    fn round_trips_through_decode() {
        let tile = Tile {
            layers: vec![Layer {
                name: "roads".to_owned(),
                version: 2,
                extent: 4096,
                features: vec![
                    line_feature(
                        1,
                        &[("name", Value::from("Main")), ("lanes", Value::Int(2))],
                    ),
                    line_feature(2, &[("name", Value::from("Second"))]),
                ],
            }],
        };
        let decoded = decode_tile(&encode_tile(&tile)).unwrap();
        assert_eq!(
            decoded, tile,
            "encode -> decode is lossless for a normalized tile"
        );
    }

    #[test]
    fn sorts_layers_by_name() {
        let layer = |name: &str| Layer {
            name: name.to_owned(),
            version: 2,
            extent: 4096,
            features: vec![line_feature(1, &[])],
        };
        let tile = Tile {
            layers: vec![layer("water"), layer("roads"), layer("buildings")],
        };
        let proto = proto::Tile::decode(encode_tile(&tile).as_slice()).unwrap();
        let names: Vec<&str> = proto.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["buildings", "roads", "water"]);
    }

    #[test]
    fn skips_empty_layers() {
        let tile = Tile {
            layers: vec![
                Layer {
                    name: "empty".to_owned(),
                    version: 2,
                    extent: 4096,
                    features: vec![],
                },
                Layer {
                    name: "roads".to_owned(),
                    version: 2,
                    extent: 4096,
                    features: vec![line_feature(1, &[])],
                },
            ],
        };
        let proto = proto::Tile::decode(encode_tile(&tile).as_slice()).unwrap();
        assert_eq!(proto.layers.len(), 1);
        assert_eq!(proto.layers[0].name, "roads");
    }

    #[test]
    fn deduplicates_keys_and_values_first_seen() {
        let tile = Tile {
            layers: vec![Layer {
                name: "roads".to_owned(),
                version: 2,
                extent: 4096,
                features: vec![
                    line_feature(1, &[("class", Value::from("primary"))]),
                    // Same key and same value -> both dedup to index 0.
                    line_feature(2, &[("class", Value::from("primary"))]),
                    line_feature(3, &[("class", Value::from("secondary"))]),
                ],
            }],
        };
        let proto = proto::Tile::decode(encode_tile(&tile).as_slice()).unwrap();
        let layer = &proto.layers[0];
        assert_eq!(layer.keys, ["class"]);
        assert_eq!(layer.values.len(), 2); // primary, secondary
        assert_eq!(layer.values[0].string_value.as_deref(), Some("primary"));
        assert_eq!(layer.values[1].string_value.as_deref(), Some("secondary"));
    }

    #[test]
    fn maps_value_types_like_upstream() {
        let tile = Tile {
            layers: vec![Layer {
                name: "l".to_owned(),
                version: 2,
                extent: 4096,
                features: vec![line_feature(
                    1,
                    &[
                        ("s", Value::from("x")),
                        ("i", Value::Int(-5)),
                        ("d", Value::Double(1.5)),
                        ("b", Value::Bool(true)),
                    ],
                )],
            }],
        };
        let proto = proto::Tile::decode(encode_tile(&tile).as_slice()).unwrap();
        // Attributes are sorted by key: b, d, i, s.
        let v = &proto.layers[0].values;
        assert_eq!(v[0].bool_value, Some(true));
        assert_eq!(v[1].double_value, Some(1.5));
        assert_eq!(v[2].sint_value, Some(-5)); // integer -> sint, never int/uint
        assert_eq!(v[3].string_value.as_deref(), Some("x"));
        assert!(v[2].int_value.is_none() && v[2].uint_value.is_none());
    }

    #[test]
    fn omits_zero_feature_id() {
        let tile = Tile {
            layers: vec![Layer {
                name: "l".to_owned(),
                version: 2,
                extent: 4096,
                features: vec![line_feature(0, &[])],
            }],
        };
        let proto = proto::Tile::decode(encode_tile(&tile).as_slice()).unwrap();
        assert_eq!(proto.layers[0].features[0].id, None);
    }
}
