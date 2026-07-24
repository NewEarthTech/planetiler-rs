//! Hand-build a few MVT features and write a browsable `hello.mbtiles`, to
//! prove the Phase 1 output stack (MVT encode -> MBTiles) end to end.
//!
//! Run: `cargo run --example hello_mbtiles [output.mbtiles]`
//! Inspect: `sqlite3 hello.mbtiles 'select name, value from metadata;'`, or open
//! it in any MVT/MBTiles viewer.

use std::collections::BTreeMap;

use planetiler_rs::archive::{
    FieldType, MbtilesWriter, TileArchive, TileArchiveMetadata, TileCompression, VectorLayer,
};
use planetiler_rs::geom::{TileCoord, world_to_lng_lat};
use planetiler_rs::model::Value;
use planetiler_rs::mvt::{Feature, Geometry, Layer, Tile, encode_tile};

fn attrs(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "hello.mbtiles".to_owned());

    // One web-mercator tile over Monaco (upstream's default example area).
    let z = 14u8;
    let coord = TileCoord::from_lng_lat(z, 7.42, 43.73)?;

    // Hand-built features in tile-local coordinates (0..4096, Y pointing down).
    let tile = Tile {
        layers: vec![
            Layer {
                name: "road".to_owned(),
                version: 2,
                extent: 4096,
                features: vec![Feature {
                    id: 1,
                    geometry: Geometry::Lines(vec![vec![(512, 512), (2048, 2048), (3584, 1024)]]),
                    attributes: attrs(&[
                        ("class", Value::from("primary")),
                        ("name", Value::from("Boulevard")),
                    ]),
                }],
            },
            Layer {
                name: "poi".to_owned(),
                version: 2,
                extent: 4096,
                features: vec![Feature {
                    id: 2,
                    geometry: Geometry::Points(vec![(2048, 2048)]),
                    attributes: attrs(&[("name", Value::from("Casino")), ("rank", Value::Int(1))]),
                }],
            },
            Layer {
                name: "building".to_owned(),
                version: 2,
                extent: 4096,
                // Exterior ring, clockwise in Y-down space (positive area).
                features: vec![Feature {
                    id: 3,
                    geometry: Geometry::Polygons(vec![vec![
                        (1000, 1000),
                        (1400, 1000),
                        (1400, 1400),
                        (1000, 1400),
                    ]]),
                    attributes: attrs(&[("height", Value::Double(12.5))]),
                }],
            },
        ],
    };

    let mvt = encode_tile(&tile);

    // Geographic bounds of the single tile, for the metadata.
    let (min_x, min_y, max_x, max_y) = coord.world_bounds();
    let (west, north) = world_to_lng_lat(min_x, min_y);
    let (east, south) = world_to_lng_lat(max_x, max_y);

    let mut writer = MbtilesWriter::create(&out, TileCompression::Gzip)?;
    writer.write_metadata(&TileArchiveMetadata {
        name: Some("planetiler-rs hello".to_owned()),
        description: Some("Hand-built demo tile".to_owned()),
        attribution: None,
        version: Some("1".to_owned()),
        layer_type: "overlay".to_owned(),
        format: "pbf".to_owned(),
        bounds: (west, south, east, north),
        center: (7.42, 43.73, z),
        min_zoom: z,
        max_zoom: z,
        compression: TileCompression::Gzip,
        vector_layers: vec![
            VectorLayer {
                id: "road".to_owned(),
                fields: BTreeMap::from([
                    ("class".to_owned(), FieldType::String),
                    ("name".to_owned(), FieldType::String),
                ]),
                min_zoom: Some(z),
                max_zoom: Some(z),
                description: None,
            },
            VectorLayer {
                id: "poi".to_owned(),
                fields: BTreeMap::from([
                    ("name".to_owned(), FieldType::String),
                    ("rank".to_owned(), FieldType::Number),
                ]),
                min_zoom: Some(z),
                max_zoom: Some(z),
                description: None,
            },
            VectorLayer {
                id: "building".to_owned(),
                fields: BTreeMap::from([("height".to_owned(), FieldType::Number)]),
                min_zoom: Some(z),
                max_zoom: Some(z),
                description: None,
            },
        ],
    })?;
    writer.write_tile(coord, &mvt)?;
    writer.finish()?;

    println!(
        "Wrote {out}: tile {}/{}/{}, {} layers, {} bytes of MVT.",
        coord.z(),
        coord.x(),
        coord.y(),
        tile.layers.len(),
        mvt.len(),
    );
    println!("Inspect: sqlite3 {out} 'select name, value from metadata;'");
    Ok(())
}
