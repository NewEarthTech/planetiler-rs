//! MVT encode/decode throughput — mirrors upstream `BenchmarkVarInt` (the
//! zigzag/command geometry stream) plus whole-tile encode/decode.

use std::collections::BTreeMap;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use planetiler_rs::model::Value;
use planetiler_rs::mvt::{Feature, Geometry, Layer, Tile, decode_tile, encode_tile, geometry};

fn zigzag_line(n: i32) -> Vec<(i32, i32)> {
    (0..n).map(|i| ((i * 3) % 4096, (i * 7) % 4096)).collect()
}

fn sample_tile(feature_count: usize) -> Tile {
    let features = (0..feature_count)
        .map(|i| Feature {
            id: i as u64 + 1,
            geometry: Geometry::Lines(vec![zigzag_line(64)]),
            attributes: BTreeMap::from([
                ("class".to_owned(), Value::from("primary")),
                ("name".to_owned(), Value::from(format!("road {i}"))),
                ("lanes".to_owned(), Value::Int((i % 4) as i64)),
            ]),
        })
        .collect();
    Tile {
        layers: vec![Layer {
            name: "road".to_owned(),
            version: 2,
            extent: 4096,
            features,
        }],
    }
}

fn bench_geometry(c: &mut Criterion) {
    let lines = vec![zigzag_line(1000)];
    c.bench_function("geometry_encode_line_1000", |b| {
        b.iter(|| geometry::encode_lines(black_box(&lines)))
    });
    let encoded = geometry::encode_lines(&lines);
    c.bench_function("geometry_decode_line_1000", |b| {
        b.iter(|| geometry::decode_lines(black_box(&encoded)).unwrap())
    });
}

fn bench_tile(c: &mut Criterion) {
    let tile = sample_tile(100);
    c.bench_function("tile_encode_100_features", |b| {
        b.iter(|| encode_tile(black_box(&tile)))
    });
    let bytes = encode_tile(&tile);
    c.bench_function("tile_decode_100_features", |b| {
        b.iter(|| decode_tile(black_box(&bytes)).unwrap())
    });
}

criterion_group!(benches, bench_geometry, bench_tile);
criterion_main!(benches);
