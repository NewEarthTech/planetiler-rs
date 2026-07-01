//! MBTiles archive writer (SQLite, compact/dedup schema).

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, params};

use super::metadata::TileArchiveMetadata;
use super::{TileArchive, TileCompression};
use crate::error::Result;
use crate::geom::TileCoord;

/// MBTiles `application_id` written to the SQLite header — `0x4d504258` = "MPBX",
/// the value upstream uses.
const APPLICATION_ID: i32 = 0x4d50_4258;

/// The compact/dedup MBTiles schema (the upstream default): a `tiles_shallow`
/// index table + a `tiles_data` blob table joined by a `tiles` view, plus the
/// `metadata` table. DDL matches Planetiler verbatim.
const SCHEMA_SQL: &str = "\
create table metadata (name text, value text);
create unique index name on metadata (name);
create table tiles_shallow (
  zoom_level integer,
  tile_column integer,
  tile_row integer,
  tile_data_id integer,
  primary key(zoom_level,tile_column,tile_row)
) without rowid;
create table tiles_data (
  tile_data_id integer primary key,
  tile_data blob
);
create view tiles AS
select
  tiles_shallow.zoom_level as zoom_level,
  tiles_shallow.tile_column as tile_column,
  tiles_shallow.tile_row as tile_row,
  tiles_data.tile_data as tile_data
from tiles_shallow
join tiles_data on tiles_shallow.tile_data_id = tiles_data.tile_data_id;
";

/// Writes tiles and metadata to an MBTiles (SQLite) file.
///
/// Tiles are gzip-compressed (or not) per [`TileCompression`], stored TMS
/// (Y-flipped), and deduplicated: identical compressed blobs share one
/// `tiles_data` row. (Upstream only deduplicates tiles it judges likely to
/// repeat; deduplicating all identical tiles yields the same readable output
/// via the `tiles` view, with a possibly smaller `tiles_data` table.)
pub struct MbtilesWriter {
    conn: Connection,
    compression: TileCompression,
    next_data_id: i64,
    dedup: HashMap<Vec<u8>, i64>,
}

impl MbtilesWriter {
    /// Create a fresh MBTiles file at `path` with the compact schema. The path
    /// must not already contain MBTiles tables (the caller handles `--force`).
    pub fn create(path: impl AsRef<Path>, compression: TileCompression) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(&format!(
            "PRAGMA application_id = {APPLICATION_ID};
             PRAGMA journal_mode = OFF;
             PRAGMA synchronous = OFF;"
        ))?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self {
            conn,
            compression,
            next_data_id: 1,
            dedup: HashMap::new(),
        })
    }

    /// Write all standard metadata rows.
    pub fn write_metadata(&mut self, metadata: &TileArchiveMetadata) -> Result<()> {
        for (key, value) in metadata.rows() {
            self.set_metadata(&key, &value)?;
        }
        Ok(())
    }
}

impl TileArchive for MbtilesWriter {
    fn write_tile(&mut self, coord: TileCoord, mvt: &[u8]) -> Result<()> {
        let compressed = self.compression.compress(mvt)?;

        let data_id = match self.dedup.get(&compressed) {
            Some(&id) => id,
            None => {
                let id = self.next_data_id;
                self.next_data_id += 1;
                self.conn.execute(
                    "INSERT OR IGNORE INTO tiles_data (tile_data_id, tile_data) VALUES (?1, ?2)",
                    params![id, compressed],
                )?;
                self.dedup.insert(compressed, id);
                id
            }
        };

        // MBTiles stores TMS: flip the Y row within the zoom level.
        let tms_y = (1u32 << coord.z()) - 1 - coord.y();
        self.conn.execute(
            "INSERT INTO tiles_shallow (zoom_level, tile_column, tile_row, tile_data_id) \
             VALUES (?1, ?2, ?3, ?4)",
            params![coord.z(), coord.x(), tms_y, data_id],
        )?;
        Ok(())
    }

    fn set_metadata(&mut self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO metadata (name, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        // Writes are autocommitted (journal_mode OFF); nothing to flush.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::metadata::{FieldType, VectorLayer};
    use crate::mvt::{Feature, Geometry, Layer, Tile, decode_tile, encode_tile};
    use std::collections::BTreeMap;

    fn sample_tile() -> Vec<u8> {
        let mut attributes = BTreeMap::new();
        attributes.insert("class".to_owned(), crate::model::Value::from("primary"));
        encode_tile(&Tile {
            layers: vec![Layer {
                name: "roads".to_owned(),
                version: 2,
                extent: 4096,
                features: vec![Feature {
                    id: 1,
                    geometry: Geometry::Lines(vec![vec![(0, 0), (100, 100)]]),
                    attributes,
                }],
            }],
        })
    }

    #[test]
    fn writes_readable_mbtiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.mbtiles");

        let mvt = sample_tile();
        let coord = TileCoord::new(14, 8000, 5000).unwrap();

        let mut writer = MbtilesWriter::create(&path, TileCompression::Gzip).unwrap();
        writer
            .write_metadata(&TileArchiveMetadata {
                name: Some("test".to_owned()),
                description: None,
                attribution: None,
                version: None,
                layer_type: "baselayer".to_owned(),
                format: "pbf".to_owned(),
                bounds: (-180.0, -85.05113, 180.0, 85.05113),
                center: (0.0, 0.0, 0),
                min_zoom: 14,
                max_zoom: 14,
                compression: TileCompression::Gzip,
                vector_layers: vec![VectorLayer {
                    id: "roads".to_owned(),
                    fields: BTreeMap::from([("class".to_owned(), FieldType::String)]),
                    min_zoom: Some(14),
                    max_zoom: Some(14),
                    description: None,
                }],
            })
            .unwrap();
        writer.write_tile(coord, &mvt).unwrap();
        writer.finish().unwrap();
        drop(writer);

        // Reopen and verify structure + content independently.
        let conn = Connection::open(&path).unwrap();

        let app_id: i32 = conn
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .unwrap();
        assert_eq!(app_id, APPLICATION_ID);

        let format: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE name = 'format'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(format, "pbf");

        // Read the tile back through the `tiles` view using TMS coordinates.
        let tms_y = (1u32 << coord.z()) - 1 - coord.y();
        let blob: Vec<u8> = conn
            .query_row(
                "SELECT tile_data FROM tiles WHERE zoom_level = ?1 AND tile_column = ?2 AND tile_row = ?3",
                params![coord.z(), coord.x(), tms_y],
                |r| r.get(0),
            )
            .unwrap();

        // Decompress (gzip) and decode; it must match what we encoded.
        let mut decoder = flate2::read::GzDecoder::new(blob.as_slice());
        let mut raw = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut raw).unwrap();
        let decoded = decode_tile(&raw).unwrap();
        assert_eq!(decoded, decode_tile(&mvt).unwrap());
    }

    #[test]
    fn deduplicates_identical_tiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedup.mbtiles");
        let mvt = sample_tile();

        let mut writer = MbtilesWriter::create(&path, TileCompression::Gzip).unwrap();
        for x in 0..3 {
            writer
                .write_tile(TileCoord::new(14, 8000 + x, 5000).unwrap(), &mvt)
                .unwrap();
        }
        writer.finish().unwrap();
        drop(writer);

        let conn = Connection::open(&path).unwrap();
        let shallow: i64 = conn
            .query_row("SELECT count(*) FROM tiles_shallow", [], |r| r.get(0))
            .unwrap();
        let data: i64 = conn
            .query_row("SELECT count(*) FROM tiles_data", [], |r| r.get(0))
            .unwrap();
        assert_eq!(shallow, 3, "three tile positions");
        assert_eq!(data, 1, "identical blobs stored once");
    }
}
