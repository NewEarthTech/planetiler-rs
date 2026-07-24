//! Shapefile source reader (`.shp` + `.dbf`, the water-polygons / Natural
//! Earth input path).
//!
//! Streams each shape with its dBASE attributes as a [`SourceFeature`].
//! Coordinates are assumed WGS84 lon/lat unless the sidecar `.prj` declares a
//! Web-Mercator projection, in which case the inverse Mercator transform is
//! applied (the OSM water-polygons distribution ships in EPSG:3857).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use geo::MapCoords;
use geo_types::Coord;
use shapefile::dbase::FieldValue;

use super::{FeatureSink, SourceFeature};
use crate::error::{Error, Result};
use crate::model::Value;

/// Stream a shapefile's features into `sink`.
///
/// # Errors
/// Returns [`Error::SourceDetail`] if the shapefile cannot be read.
pub fn stream_shapefile(path: &Path, sink: &mut FeatureSink) -> Result<()> {
    let mercator = is_web_mercator(path);
    let mut reader = shapefile::Reader::from_path(path).map_err(|e| Error::SourceDetail {
        context: "cannot open shapefile",
        message: e.to_string(),
    })?;

    for (index, shape_record) in reader.iter_shapes_and_records().enumerate() {
        let (shape, record) = shape_record.map_err(|e| Error::SourceDetail {
            context: "failed to read shapefile record",
            message: e.to_string(),
        })?;
        if matches!(shape, shapefile::Shape::NullShape) {
            continue;
        }
        let geometry = match geo_types::Geometry::<f64>::try_from(shape) {
            Ok(geometry) => geometry,
            Err(_) => continue, // unsupported shape type (e.g. multipatch)
        };
        let geometry = if mercator {
            geometry.map_coords(inverse_mercator)
        } else {
            geometry
        };

        let mut tags = BTreeMap::new();
        for (name, value) in record.into_iter() {
            if let Some(value) = convert_field(value) {
                tags.insert(name, value);
            }
        }

        sink(SourceFeature {
            id: index as u64,
            geometry: Arc::new(geometry),
            tags: Arc::new(tags),
            relations: Vec::new(),
        })?;
    }
    Ok(())
}

/// Whether the sidecar `.prj` declares a Web-Mercator projection.
fn is_web_mercator(path: &Path) -> bool {
    let prj = path.with_extension("prj");
    match std::fs::read_to_string(prj) {
        Ok(wkt) => {
            wkt.contains("3857") || wkt.contains("Pseudo-Mercator") || wkt.contains("Mercator_1SP")
        }
        Err(_) => false,
    }
}

/// EPSG:3857 metres → WGS84 lon/lat degrees.
fn inverse_mercator(c: Coord<f64>) -> Coord<f64> {
    const EARTH_RADIUS: f64 = 6_378_137.0;
    Coord {
        x: (c.x / EARTH_RADIUS).to_degrees(),
        y: (c.y / EARTH_RADIUS).sinh().atan().to_degrees(),
    }
}

/// Map a dBASE field to a tag value; empty/null fields are omitted.
fn convert_field(value: FieldValue) -> Option<Value> {
    match value {
        FieldValue::Character(Some(s)) if !s.trim().is_empty() => Some(Value::from(s)),
        FieldValue::Character(_) => None,
        FieldValue::Numeric(Some(n)) => Some(Value::Double(n)),
        FieldValue::Numeric(None) => None,
        FieldValue::Float(Some(f)) => Some(Value::Double(f64::from(f))),
        FieldValue::Float(None) => None,
        FieldValue::Integer(i) => Some(Value::Int(i64::from(i))),
        FieldValue::Double(d) => Some(Value::Double(d)),
        FieldValue::Currency(c) => Some(Value::Double(c)),
        FieldValue::Logical(Some(b)) => Some(Value::Bool(b)),
        FieldValue::Logical(None) => None,
        FieldValue::Date(Some(d)) => Some(Value::from(d.to_string())),
        FieldValue::Date(None) => None,
        FieldValue::DateTime(dt) => Some(Value::from(format!("{dt:?}"))),
        FieldValue::Memo(m) => Some(Value::from(m)),
    }
}

#[cfg(test)]
mod tests {
    use shapefile::dbase::{Record, TableWriterBuilder};
    use shapefile::{Point, Polygon, PolygonRing, Polyline};

    use super::*;

    fn collect(path: &Path) -> Vec<SourceFeature> {
        let mut features = Vec::new();
        stream_shapefile(path, &mut |f| {
            features.push(f);
            Ok(())
        })
        .unwrap();
        features
    }

    fn table() -> TableWriterBuilder {
        TableWriterBuilder::new()
            .add_character_field("name".try_into().unwrap(), 20)
            .add_numeric_field("rank".try_into().unwrap(), 8, 2)
    }

    fn record(name: &str, rank: Option<f64>) -> Record {
        let mut record = Record::default();
        record.insert(
            "name".to_owned(),
            FieldValue::Character(Some(name.to_owned())),
        );
        record.insert("rank".to_owned(), FieldValue::Numeric(rank));
        record
    }

    #[test]
    fn reads_polygons_with_attributes() {
        // Shapefiles hold a single geometry type per file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("polys.shp");
        let mut writer = shapefile::Writer::from_path(&path, table()).unwrap();
        let polygon = Polygon::new(PolygonRing::Outer(vec![
            Point::new(0.0, 0.0),
            Point::new(0.0, 1.0),
            Point::new(1.0, 1.0),
            Point::new(1.0, 0.0),
            Point::new(0.0, 0.0),
        ]));
        writer
            .write_shape_and_record(&polygon, &record("water", Some(2.0)))
            .unwrap();
        drop(writer);

        let features = collect(&path);
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].tag("name"), Some(&Value::from("water")));
        assert_eq!(features[0].tag("rank"), Some(&Value::Double(2.0)));
        assert!(matches!(
            *features[0].geometry,
            geo_types::Geometry::MultiPolygon(_)
        ));
    }

    #[test]
    fn reads_lines_and_omits_null_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lines.shp");
        let mut writer = shapefile::Writer::from_path(&path, table()).unwrap();
        let line = Polyline::new(vec![Point::new(2.0, 2.0), Point::new(3.0, 3.0)]);
        writer
            .write_shape_and_record(&line, &record("river", None))
            .unwrap();
        drop(writer);

        let features = collect(&path);
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].tag("name"), Some(&Value::from("river")));
        assert_eq!(features[0].tag("rank"), None, "null numeric omitted");
        assert!(matches!(
            *features[0].geometry,
            geo_types::Geometry::MultiLineString(_)
        ));
    }

    #[test]
    fn reprojects_web_mercator_when_prj_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("merc.shp");
        let table = TableWriterBuilder::new().add_character_field("name".try_into().unwrap(), 10);
        let mut writer = shapefile::Writer::from_path(&path, table).unwrap();
        let mut record = Record::default();
        record.insert(
            "name".to_owned(),
            FieldValue::Character(Some("p".to_owned())),
        );
        // Web-Mercator coordinates of approximately (1°, 2°).
        let point = Point::new(111_319.490_793, 222_684.208_506);
        writer.write_shape_and_record(&point, &record).unwrap();
        drop(writer);
        std::fs::write(
            path.with_extension("prj"),
            "PROJCS[\"WGS 84 / Pseudo-Mercator\",...,AUTHORITY[\"EPSG\",\"3857\"]]",
        )
        .unwrap();

        let features = collect(&path);
        assert_eq!(features.len(), 1);
        let geo_types::Geometry::Point(p) = &*features[0].geometry else {
            panic!("expected point");
        };
        assert!((p.x() - 1.0).abs() < 1e-6, "lon {}", p.x());
        assert!((p.y() - 2.0).abs() < 1e-6, "lat {}", p.y());
    }
}
