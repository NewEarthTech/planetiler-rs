//! MVT tile decoding for round-trip and parity checks.
//!
//! Resolves the wire-level key/value dictionaries into concrete geometry and
//! attributes, so two tiles can be compared semantically regardless of how each
//! encoder ordered its dictionaries. This is the correctness oracle for
//! head-to-heads against upstream Planetiler output.

use std::collections::BTreeMap;

use prost::Message;

use super::geometry::{decode_lines, decode_points, decode_polygons};
use super::proto;
use super::proto::tile::GeomType;
use crate::error::{Error, Result};
use crate::model::Value;

/// A structured, in-memory MVT tile: layers resolved into concrete geometry and
/// attributes. Produced by [`decode_tile`] and consumed by the encoder, so
/// `encode` and `decode` share one representation.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    /// Layers in wire order (see [`semantically_equal`] for order-insensitive
    /// comparison).
    pub layers: Vec<Layer>,
}

/// A structured MVT layer.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    /// Layer name.
    pub name: String,
    /// MVT version (2 for spec 2.1).
    pub version: u32,
    /// Tile extent (grid units per edge; default 4096).
    pub extent: u32,
    /// Features in wire order (draw order).
    pub features: Vec<Feature>,
}

/// A structured MVT feature.
#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    /// Feature id (0 if unset).
    pub id: u64,
    /// Geometry in tile coordinates.
    pub geometry: Geometry,
    /// Attributes keyed by name. The `BTreeMap` keeps them sorted, which is also
    /// the deterministic tag order the encoder emits.
    pub attributes: BTreeMap<String, Value>,
}

/// Structured MVT geometry, grouped by geometry type.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Geometry {
    /// One or more points.
    Points(Vec<(i32, i32)>),
    /// One or more line strings.
    Lines(Vec<Vec<(i32, i32)>>),
    /// One or more polygon rings (without repeated closing vertices).
    Polygons(Vec<Vec<(i32, i32)>>),
    /// `UNKNOWN` geometry type.
    Unknown,
}

/// Decode raw (already-decompressed) MVT protobuf bytes into a [`Tile`].
///
/// # Errors
/// Returns [`Error::MvtDecode`] on protobuf-level failures or malformed tags,
/// and [`Error::MvtGeometry`] on malformed geometry command streams.
pub fn decode_tile(bytes: &[u8]) -> Result<Tile> {
    let tile =
        proto::Tile::decode(bytes).map_err(|_| Error::MvtDecode("protobuf decode failed"))?;
    let mut layers = Vec::with_capacity(tile.layers.len());
    for layer in &tile.layers {
        layers.push(decode_layer(layer)?);
    }
    Ok(Tile { layers })
}

fn decode_layer(layer: &proto::tile::Layer) -> Result<Layer> {
    let mut features = Vec::with_capacity(layer.features.len());
    for feature in &layer.features {
        features.push(decode_feature(feature, layer)?);
    }
    Ok(Layer {
        name: layer.name.clone(),
        version: layer.version,
        extent: layer.extent.unwrap_or(4096),
        features,
    })
}

fn decode_feature(feature: &proto::tile::Feature, layer: &proto::tile::Layer) -> Result<Feature> {
    if !feature.tags.len().is_multiple_of(2) {
        return Err(Error::MvtDecode("odd number of feature tags"));
    }
    let mut attributes = BTreeMap::new();
    for pair in feature.tags.chunks_exact(2) {
        let key = layer
            .keys
            .get(pair[0] as usize)
            .ok_or(Error::MvtDecode("tag key index out of range"))?;
        let value = layer
            .values
            .get(pair[1] as usize)
            .ok_or(Error::MvtDecode("tag value index out of range"))?;
        attributes.insert(key.clone(), decode_value(value)?);
    }

    let geom_type = GeomType::try_from(feature.r#type.unwrap_or(0)).unwrap_or(GeomType::Unknown);
    let geometry = match geom_type {
        GeomType::Point => Geometry::Points(decode_points(&feature.geometry)?),
        GeomType::Linestring => Geometry::Lines(decode_lines(&feature.geometry)?),
        GeomType::Polygon => Geometry::Polygons(decode_polygons(&feature.geometry)?),
        GeomType::Unknown => Geometry::Unknown,
    };

    Ok(Feature {
        id: feature.id.unwrap_or(0),
        geometry,
        attributes,
    })
}

fn decode_value(value: &proto::tile::Value) -> Result<Value> {
    if let Some(s) = &value.string_value {
        return Ok(Value::String(s.clone()));
    }
    if let Some(x) = value.float_value {
        return Ok(Value::Float(x));
    }
    if let Some(x) = value.double_value {
        return Ok(Value::Double(x));
    }
    if let Some(x) = value.int_value {
        return Ok(Value::Int(x));
    }
    if let Some(x) = value.uint_value {
        return Ok(Value::Uint(x));
    }
    if let Some(x) = value.sint_value {
        return Ok(Value::Int(x));
    }
    if let Some(x) = value.bool_value {
        return Ok(Value::Bool(x));
    }
    Err(Error::MvtDecode("empty MVT value"))
}

/// Semantic tile equality: same layers (matched by name, order-insensitive),
/// each with the same features in order (feature id, geometry, and attributes).
///
/// The wire-level key/value dictionary ordering and layer ordering are not
/// semantically meaningful, so this — not a raw byte compare — is the right
/// check for parity against upstream, given that byte-identical output is
/// blocked by gzip and Java `HashMap` tag ordering (see the Phase 1 parity
/// notes).
pub fn semantically_equal(a: &Tile, b: &Tile) -> bool {
    if a.layers.len() != b.layers.len() {
        return false;
    }
    let mut a_layers: Vec<&Layer> = a.layers.iter().collect();
    let mut b_layers: Vec<&Layer> = b.layers.iter().collect();
    a_layers.sort_by(|l, r| l.name.cmp(&r.name));
    b_layers.sort_by(|l, r| l.name.cmp(&r.name));
    a_layers == b_layers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::geometry::encode_lines;

    fn sval(s: &str) -> proto::tile::Value {
        proto::tile::Value {
            string_value: Some(s.to_owned()),
            ..Default::default()
        }
    }

    fn roads_layer(
        keys: Vec<&str>,
        values: Vec<proto::tile::Value>,
        tags: Vec<u32>,
    ) -> proto::tile::Layer {
        proto::tile::Layer {
            version: 2,
            name: "roads".to_owned(),
            features: vec![proto::tile::Feature {
                id: Some(1),
                tags,
                r#type: Some(GeomType::Linestring as i32),
                geometry: encode_lines(&[vec![(0, 0), (10, 10)]]),
            }],
            keys: keys.into_iter().map(str::to_owned).collect(),
            values,
            extent: Some(4096),
        }
    }

    #[test]
    fn decodes_layer_feature_and_attributes() {
        let tile = proto::Tile {
            layers: vec![roads_layer(
                vec!["name", "class"],
                vec![sval("Main"), sval("primary")],
                vec![0, 0, 1, 1],
            )],
        };
        let decoded = decode_tile(&tile.encode_to_vec()).unwrap();

        assert_eq!(decoded.layers.len(), 1);
        let layer = &decoded.layers[0];
        assert_eq!(layer.name, "roads");
        assert_eq!(layer.version, 2);
        assert_eq!(layer.extent, 4096);

        let f = &layer.features[0];
        assert_eq!(f.id, 1);
        assert_eq!(f.geometry, Geometry::Lines(vec![vec![(0, 0), (10, 10)]]));
        assert_eq!(
            f.attributes.get("name"),
            Some(&Value::String("Main".into()))
        );
        assert_eq!(
            f.attributes.get("class"),
            Some(&Value::String("primary".into()))
        );
    }

    #[test]
    fn dictionary_order_does_not_change_decoding() {
        // Same content, but the keys/values tables (and tag indices) reordered.
        let a = proto::Tile {
            layers: vec![roads_layer(
                vec!["name", "class"],
                vec![sval("Main"), sval("primary")],
                vec![0, 0, 1, 1],
            )],
        };
        let b = proto::Tile {
            layers: vec![roads_layer(
                vec!["class", "name"],
                vec![sval("primary"), sval("Main")],
                vec![1, 1, 0, 0],
            )],
        };
        let da = decode_tile(&a.encode_to_vec()).unwrap();
        let db = decode_tile(&b.encode_to_vec()).unwrap();
        assert_eq!(da, db, "resolved tiles are identical despite table order");
        assert!(semantically_equal(&da, &db));
    }

    #[test]
    fn semantic_equality_ignores_layer_order() {
        let water = proto::tile::Layer {
            version: 2,
            name: "water".to_owned(),
            features: vec![],
            keys: vec![],
            values: vec![],
            extent: Some(4096),
        };
        let roads = roads_layer(vec!["name"], vec![sval("Main")], vec![0, 0]);

        let ab = Tile {
            layers: decode_tile(
                &proto::Tile {
                    layers: vec![roads.clone(), water.clone()],
                }
                .encode_to_vec(),
            )
            .unwrap()
            .layers,
        };
        let ba = Tile {
            layers: decode_tile(
                &proto::Tile {
                    layers: vec![water, roads],
                }
                .encode_to_vec(),
            )
            .unwrap()
            .layers,
        };

        assert!(semantically_equal(&ab, &ba));
        assert_ne!(
            ab, ba,
            "raw equality is layer-order sensitive; semantic equality is not"
        );
    }

    #[test]
    fn rejects_out_of_range_tag_index() {
        let bad = proto::Tile {
            layers: vec![roads_layer(vec!["name"], vec![sval("Main")], vec![0, 5])],
        };
        assert!(decode_tile(&bad.encode_to_vec()).is_err());
    }
}
