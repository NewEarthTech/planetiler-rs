//! MBTiles write throughput — mirrors upstream `BenchmarkMbtilesWriter`. Each
//! iteration writes a batch of tiles (gzip + dedup + shallow insert) to a fresh
//! temp database.

use std::collections::BTreeMap;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use planetiler_rs::archive::{MbtilesWriter, TileArchive, TileCompression};
use planetiler_rs::geom::TileCoord;
use planetiler_rs::model::Value;
use planetiler_rs::mvt::{Feature, Geometry, Layer, Tile, encode_tile};

fn sample_mvt() -> Vec<u8> {
    encode_tile(&Tile {
        layers: vec![Layer {
            name: "road".to_owned(),
            version: 2,
            extent: 4096,
            features: vec![Feature {
                id: 1,
                geometry: Geometry::Lines(vec![vec![(0, 0), (2048, 2048), (4096, 0)]]),
                attributes: BTreeMap::from([("class".to_owned(), Value::from("primary"))]),
            }],
        }],
    })
}

fn bench_write(c: &mut Criterion) {
    let mvt = sample_mvt();
    const TILES: u32 = 500;
    c.bench_function("mbtiles_write_500_tiles", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bench.mbtiles");
            let mut writer = MbtilesWriter::create(&path, TileCompression::Gzip).unwrap();
            for i in 0..TILES {
                let coord = TileCoord::new(14, 8000 + (i % 100), 5000 + (i / 100)).unwrap();
                writer.write_tile(coord, black_box(&mvt)).unwrap();
            }
            writer.finish().unwrap();
        })
    });
}

criterion_group!(benches, bench_write);
criterion_main!(benches);
