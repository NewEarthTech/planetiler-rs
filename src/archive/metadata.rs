//! Archive metadata: the MBTiles `metadata` table rows and the tilejson
//! `json`/`vector_layers` value, mirroring upstream Planetiler.

use std::collections::BTreeMap;

use serde_json::json;

use super::TileCompression;

/// Attribute field type reported in tilejson `vector_layers[].fields`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// Numeric attribute.
    Number,
    /// Boolean attribute.
    Boolean,
    /// String attribute (also the widened type for mixed-type attributes).
    String,
}

impl FieldType {
    /// The tilejson string (`Number` / `Boolean` / `String`).
    pub fn as_str(self) -> &'static str {
        match self {
            FieldType::Number => "Number",
            FieldType::Boolean => "Boolean",
            FieldType::String => "String",
        }
    }
}

/// One entry in tilejson `vector_layers`.
#[derive(Debug, Clone)]
pub struct VectorLayer {
    /// Layer name.
    pub id: String,
    /// Attribute name → field type.
    pub fields: BTreeMap<String, FieldType>,
    /// Lowest zoom the layer appears at, if known.
    pub min_zoom: Option<u8>,
    /// Highest zoom the layer appears at, if known.
    pub max_zoom: Option<u8>,
    /// Optional human description.
    pub description: Option<String>,
}

/// Archive metadata, written to the MBTiles `metadata` table. Field names and
/// value formatting mirror Planetiler's `TileArchiveMetadata`.
#[derive(Debug, Clone)]
pub struct TileArchiveMetadata {
    /// `name` row (omitted if `None`).
    pub name: Option<String>,
    /// `description` row (omitted if `None`).
    pub description: Option<String>,
    /// `attribution` row (omitted if `None`).
    pub attribution: Option<String>,
    /// `version` row (omitted if `None`).
    pub version: Option<String>,
    /// `type` row (`baselayer` / `overlay`).
    pub layer_type: String,
    /// `format` row (`pbf` for MVT).
    pub format: String,
    /// Geographic bounds `(west, south, east, north)` in degrees.
    pub bounds: (f64, f64, f64, f64),
    /// Map center `(lon, lat, zoom)`.
    pub center: (f64, f64, u8),
    /// `minzoom` row.
    pub min_zoom: u8,
    /// `maxzoom` row.
    pub max_zoom: u8,
    /// `compression` row.
    pub compression: TileCompression,
    /// Layers for the `json` row's `vector_layers`.
    pub vector_layers: Vec<VectorLayer>,
}

impl TileArchiveMetadata {
    /// The `(name, value)` rows for the `metadata` table. Optional string fields
    /// are omitted when absent (NON_ABSENT), matching upstream.
    pub fn rows(&self) -> Vec<(String, String)> {
        let mut rows: Vec<(String, String)> = Vec::new();
        if let Some(v) = &self.name {
            rows.push(("name".to_owned(), v.clone()));
        }
        if let Some(v) = &self.description {
            rows.push(("description".to_owned(), v.clone()));
        }
        if let Some(v) = &self.attribution {
            rows.push(("attribution".to_owned(), v.clone()));
        }
        if let Some(v) = &self.version {
            rows.push(("version".to_owned(), v.clone()));
        }
        rows.push(("type".to_owned(), self.layer_type.clone()));
        rows.push(("format".to_owned(), self.format.clone()));
        rows.push((
            "bounds".to_owned(),
            join_coords(&[self.bounds.0, self.bounds.1, self.bounds.2, self.bounds.3]),
        ));
        rows.push((
            "center".to_owned(),
            format!(
                "{},{},{}",
                fmt5(self.center.0),
                fmt5(self.center.1),
                self.center.2
            ),
        ));
        rows.push(("minzoom".to_owned(), self.min_zoom.to_string()));
        rows.push(("maxzoom".to_owned(), self.max_zoom.to_string()));
        rows.push((
            "compression".to_owned(),
            self.compression.as_str().to_owned(),
        ));
        rows.push(("json".to_owned(), self.json_string()));
        rows
    }

    /// The `json` metadata value: `{"vector_layers":[…]}`, layers sorted by id.
    fn json_string(&self) -> String {
        let mut layers = self.vector_layers.clone();
        layers.sort_by(|a, b| a.id.cmp(&b.id));
        let layers: Vec<serde_json::Value> = layers.iter().map(vector_layer_json).collect();
        json!({ "vector_layers": layers }).to_string()
    }
}

fn vector_layer_json(layer: &VectorLayer) -> serde_json::Value {
    let fields: BTreeMap<&str, &str> = layer
        .fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut obj = serde_json::Map::new();
    obj.insert("id".to_owned(), json!(layer.id));
    obj.insert("fields".to_owned(), json!(fields));
    if let Some(d) = &layer.description
        && !d.is_empty()
    {
        obj.insert("description".to_owned(), json!(d));
    }
    if let Some(z) = layer.min_zoom {
        obj.insert("minzoom".to_owned(), json!(z));
    }
    if let Some(z) = layer.max_zoom {
        obj.insert("maxzoom".to_owned(), json!(z));
    }
    serde_json::Value::Object(obj)
}

/// Format a coordinate to at most 5 decimals, trimming trailing zeros (US
/// locale, no grouping), matching Planetiler's `Format.joinCoordinates`.
fn fmt5(v: f64) -> String {
    let s = format!("{v:.5}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" || trimmed == "-0" {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn join_coords(coords: &[f64]) -> String {
    coords
        .iter()
        .map(|c| fmt5(*c))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TileArchiveMetadata {
        let mut fields = BTreeMap::new();
        fields.insert("class".to_owned(), FieldType::String);
        fields.insert("lanes".to_owned(), FieldType::Number);
        TileArchiveMetadata {
            name: Some("test".to_owned()),
            description: None,
            attribution: None,
            version: None,
            layer_type: "baselayer".to_owned(),
            format: "pbf".to_owned(),
            bounds: (-180.0, -85.051_13, 180.0, 85.051_13),
            center: (0.0, 0.0, 0),
            min_zoom: 0,
            max_zoom: 14,
            compression: TileCompression::Gzip,
            vector_layers: vec![VectorLayer {
                id: "roads".to_owned(),
                fields,
                min_zoom: Some(0),
                max_zoom: Some(14),
                description: None,
            }],
        }
    }

    #[test]
    fn formats_standard_rows() {
        let rows: BTreeMap<String, String> = sample().rows().into_iter().collect();
        assert_eq!(rows["name"], "test");
        assert_eq!(rows["type"], "baselayer");
        assert_eq!(rows["format"], "pbf");
        assert_eq!(rows["bounds"], "-180,-85.05113,180,85.05113");
        assert_eq!(rows["center"], "0,0,0");
        assert_eq!(rows["minzoom"], "0");
        assert_eq!(rows["maxzoom"], "14");
        assert_eq!(rows["compression"], "gzip");
        assert!(
            !rows.contains_key("description"),
            "absent fields are omitted"
        );
    }

    #[test]
    fn json_row_has_vector_layers() {
        let rows: BTreeMap<String, String> = sample().rows().into_iter().collect();
        let parsed: serde_json::Value = serde_json::from_str(&rows["json"]).unwrap();
        let layer = &parsed["vector_layers"][0];
        assert_eq!(layer["id"], "roads");
        assert_eq!(layer["fields"]["class"], "String");
        assert_eq!(layer["fields"]["lanes"], "Number");
        assert_eq!(layer["minzoom"], 0);
        assert_eq!(layer["maxzoom"], 14);
        assert!(layer.get("description").is_none());
    }

    #[test]
    fn trims_coordinate_decimals() {
        assert_eq!(fmt5(-180.0), "-180");
        assert_eq!(fmt5(85.0511287), "85.05113");
        assert_eq!(fmt5(0.0), "0");
        assert_eq!(fmt5(-0.0), "0");
        assert_eq!(fmt5(7.4), "7.4");
    }
}
