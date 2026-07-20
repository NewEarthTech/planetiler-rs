//! Criterion micro-benchmark mirroring upstream's `BenchmarkTileCoord`.
//!
//! This is the Phase 0 seed of the `benches/` suite that will grow to mirror
//! all 13 upstream `planetiler-benchmarks` timing classes, each landing with
//! the component it measures.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use planetiler_rs::geom::TileCoord;
use planetiler_rs::model::SortKey;

fn bench_tile_coord(c: &mut Criterion) {
    c.bench_function("tilecoord_from_lng_lat_z14", |b| {
        b.iter(|| {
            TileCoord::from_lng_lat(black_box(14), black_box(7.42), black_box(43.73)).unwrap()
        })
    });

    let tile = TileCoord::from_lng_lat(14, 7.42, 43.73).unwrap();
    c.bench_function("sortkey_encode_decode", |b| {
        b.iter(|| {
            let k = SortKey::new(black_box(tile), black_box(3), black_box(-100)).unwrap();
            black_box((k.z(), k.x(), k.y(), k.layer(), k.zorder()))
        })
    });
}

criterion_group!(benches, bench_tile_coord);
criterion_main!(benches);
