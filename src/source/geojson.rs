//! GeoJSON source reader.
//!
//! Parses a GeoJSON `FeatureCollection`, `Feature`, or bare `Geometry` into
//! [`SourceFeature`]s using the `geojson` crate's geo-types conversion.

use std::collections::BTreeMap;
use std::path::Path;

use geo_types::Geometry;

use super::SourceFeature;
use crate::error::{Error, Result};
use crate::model::Value;

/// Read all features from a GeoJSON file.
///
/// # Errors
/// Returns [`Error::Source`] if the file is not valid GeoJSON or contains a
/// geometry type geo-types cannot represent.
pub fn read_geojson(path: impl AsRef<Path>) -> Result<Vec<SourceFeature>> {
    let text = std::fs::read_to_string(path)?;
    let parsed: ::geojson::GeoJson = text.parse().map_err(|_| Error::Source("invalid GeoJSON"))?;

    let mut out = Vec::new();
    match parsed {
        ::geojson::GeoJson::FeatureCollection(fc) => {
            for (i, feature) in fc.features.into_iter().enumerate() {
                if let Some(sf) = convert_feature(feature, i as u64)? {
                    out.push(sf);
                }
            }
        }
        ::geojson::GeoJson::Feature(feature) => {
            if let Some(sf) = convert_feature(feature, 0)? {
                out.push(sf);
            }
        }
        ::geojson::GeoJson::Geometry(geometry) => {
            out.push(SourceFeature {
                id: 0,
                geometry: convert_geometry(geometry)?,
                tags: BTreeMap::new(),
            });
        }
    }
    Ok(out)
}

fn convert_feature(feature: ::geojson::Feature, fallback_id: u64) -> Result<Option<SourceFeature>> {
    let Some(geometry) = feature.geometry else {
        return Ok(None);
    };
    let geometry = convert_geometry(geometry)?;

    let mut tags = BTreeMap::new();
    if let Some(properties) = feature.properties {
        for (key, value) in properties {
            if let Some(value) = json_to_value(value) {
                tags.insert(key, value);
            }
        }
    }

    let id = match feature.id {
        Some(::geojson::feature::Id::Number(n)) => n.as_u64().unwrap_or(fallback_id),
        _ => fallback_id,
    };

    Ok(Some(SourceFeature { id, geometry, tags }))
}

fn convert_geometry(geometry: ::geojson::Geometry) -> Result<Geometry<f64>> {
    Geometry::try_from(geometry).map_err(|_| Error::Source("unsupported GeoJSON geometry"))
}

fn json_to_value(value: serde_json::Value) -> Option<Value> {
    match value {
        serde_json::Value::String(s) => Some(Value::String(s)),
        serde_json::Value::Bool(b) => Some(Value::Bool(b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Value::Int(i))
            } else if let Some(u) = n.as_u64() {
                Some(Value::Uint(u))
            } else {
                n.as_f64().map(Value::Double)
            }
        }
        // GeoJSON null/array/object properties are dropped (no MVT value type).
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(contents: &str) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    #[test]
    fn reads_feature_collection_with_typed_tags() {
        let json = r#"{
            "type": "FeatureCollection",
            "features": [
                {"type":"Feature","id":7,"properties":{"name":"A","lanes":3,"toll":true},
                 "geometry":{"type":"Point","coordinates":[7.42,43.73]}},
                {"type":"Feature","properties":{},
                 "geometry":{"type":"LineString","coordinates":[[0,0],[1,1]]}}
            ]
        }"#;
        let f = write_temp(json);
        let features = read_geojson(f.path()).unwrap();
        assert_eq!(features.len(), 2);

        assert_eq!(features[0].id, 7);
        assert_eq!(features[0].tag("name"), Some(&Value::String("A".into())));
        assert_eq!(features[0].tag("lanes"), Some(&Value::Int(3)));
        assert_eq!(features[0].tag("toll"), Some(&Value::Bool(true)));
        assert!(matches!(features[0].geometry, Geometry::Point(_)));

        // Missing id falls back to the feature index.
        assert_eq!(features[1].id, 1);
        assert!(matches!(features[1].geometry, Geometry::LineString(_)));
    }

    #[test]
    fn rejects_invalid_json() {
        let f = write_temp("{not json");
        assert!(read_geojson(f.path()).is_err());
    }
}
