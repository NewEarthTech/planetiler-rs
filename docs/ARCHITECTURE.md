# planetiler-rs architecture

A Rust port of [Planetiler](https://github.com/onthegomap/planetiler): it turns OpenStreetMap (and other geodata) into planet-scale Mapbox Vector Tile archives (MBTiles/PMTiles) through a parallel read → external-sort-on-disk → render pipeline. This document is the durable in-repo reference for how the crate is shaped and how it is proven correct and worthwhile. The phased build plan and raw upstream research live outside the repo (in local Claude context); this file is what a contributor reads.

## Why this exists

The **only** justification for rewriting Planetiler in Rust is beating the Java original on **wall-clock time** and **peak memory / OOM behavior**. So the project is *benchmark-first*: a time + peak-RSS harness is stood up before anything can win, and every upstream example is mirrored one-to-one with a checked-in head-to-head benchmark. "Is it worth it?" must be a number readable at any moment. If a capability has no benchmark, it is not done.

## Pipeline

Data flows one direction, mirroring upstream:

```
source → profile → render → collection (sort) → group → mvt (encode) → archive
 bytes   features   Entry     sorted Entries    tiles    gzipped MVT    MBTiles/PMTiles
```

- **source** — decode inputs (GeoJSON first; then two-pass OSM `.osm.pbf`; later shapefile / Natural Earth / geoparquet) into a common `SourceFeature` with lon/lat geometry + tags.
- **profile** — user schema: `Profile::process_feature(SourceFeature, FeatureCollector)` emits zoom-ranged vector features. Sees only lon/lat world geometry and `model`.
- **render** — project to tile space, simplify in tile units, clip, slice into every covering tile, repair snapped polygons, and emit `model::Entry` records. The hardest correctness code.
- **collection** — a generic `ExternalMergeSort<Key, Bytes>` plus the `LongLongMap` node store. Sorts `Entry` records by key without understanding them.
- **mvt** — encode grouped features to MVT (zigzag/delta commands, Y-down winding, per-tile key/value tables); decode for parity checks.
- **archive** — `TileArchive` writers (MBTiles now, PMTiles later) consuming tiles in order: per-tile gzip, whole-tile dedup, `vector_layers` metadata.
- **worker** — backpressured bounded-queue stages over crossbeam + rayon, ordered single-threaded sinks.

## Module map

| Module | Responsibility | Depends on | Status |
|--------|----------------|------------|--------|
| `error` | Crate-wide `Error`/`Result`. | — | Phase 0 |
| `geom` | Web-mercator projection, `TileCoord`, zoom & extent math. | `error` | Phase 0 |
| `model` | The data spine: `Value`, `SortKey`, `Entry`, `PlanetilerConfig`. | `geom`, `error` | Phase 0 |
| `bench` | Wall-time + peak-RSS capture for head-to-heads. | — | Phase 0 |
| `mvt` | MVT encode/decode. | `model`, `geom` | Phase 1 |
| `archive` | `TileArchive` + MBTiles/PMTiles writers. | `model`, `mvt` | Phase 1 / 5 |
| `source` | Input readers → `SourceFeature`. | `geom` | Phase 2+ |
| `profile` | `Profile` trait + `FeatureCollector`. | `source`, `model` | Phase 2 |
| `render` | `TiledGeometry`: features → tile `Entry` records. | `model`, `geom` | Phase 2 |
| `collection` | Generic external sort + `LongLongMap`. | `model` | Phase 3 |
| `worker` | Bounded-queue worker pipeline. | — | Phase 3 |
| `cli` | Command-line surface. | most of the above | grows per phase |

**Boundary rules (the spine contract).** The `SortKey` + `Entry` codec lives in `model`, never in `render`. That keeps the dependency arrow pointing *into* `model`: `render` produces `Entry` records, `collection` sorts them generically (`ExternalMergeSort<Key, Bytes>`), and `archive` consumes them in order — `collection` never depends on `render`. `profile` only ever sees lon/lat world geometry; it never touches tile space or `render`.

## The data spine

### `SortKey` — 64-bit feature ordering

Packs the fields that define rendering order so a plain ascending `u64` sort groups every feature of a tile together and orders within a tile:

| bits | field | meaning |
|-----:|-------|---------|
| 4  | `z`        | zoom, `0..=15` |
| 15 | `x`        | tile column, `0..2^z` |
| 15 | `y`        | tile row, `0..2^z` |
| 6  | `layer`    | layer index, `0..64` |
| 20 | `zorder`   | draw order (offset-binary signed) |
| 4  | `reserved` | zero (headroom) |

- **Offset-binary, not zigzag, for `zorder`.** Adding `2^19` preserves signed order in unsigned space; zigzag would interleave negatives and positives and break the sort. (Zigzag still appears later in MVT geometry commands, where compactness — not order — is the goal.)
- **Tile bits are row-major `(z, x, y)`**, the right grouping for MBTiles. PMTiles wants Hilbert clustering; since callers only use `SortKey::new` + accessors, the internal packing can switch to a Hilbert index in Phase 5 without touching them.
- **Total, stable order** comes from `Entry`, which compares `key` first, then the raw value bytes. The value tiebreak makes output reproducible and makes byte-identical records true duplicates (used by tile dedup) — mirroring upstream's `SortableFeature`.

### `Value`

Typed attribute value (`String`/`Bool`/`Int`/`Uint`/`Float`/`Double`) matching what an MVT key/value table can hold. `f32` and `f64` are kept distinct to stay at parity with upstream's encoding.

## Coordinate conventions

- **World space** is the unit square `[0, 1]²` (whole mercator globe). Multiply by `2^z` for tile space; the fractional part times the tile extent (default **4096**) gives the position within a tile.
- **Y is down** — north at `y = 0`. Consequently MVT exterior rings are **clockwise in screen space** (opposite GeoJSON). Getting winding wrong is a silent render bug.
- **XYZ vs TMS.** Tiles are addressed XYZ everywhere in code; MBTiles stores TMS (Y flipped), converted only in the archive writer.
- **Latitude clamp** to ±85.05112878° (web-mercator limit).

## Benchmark-first methodology

- **Corpus = every upstream example** (the in-tree `planetiler-examples`, the standalone single-file profiles, and the OpenMapTiles basemap), each mirrored under `examples/` and mapped to the phase that unlocks it.
- **Definition of done, per capability:** output semantically identical to upstream (canonicalized MVT diff) **and** a checked-in benchmark recording wall-time + peak RSS vs upstream Java on identical input.
- **Macro harness** (`src/bin/bench.rs` + `bench` module): times both sides under `/usr/bin/time`, captures peak RSS, and writes a comparison row to `bench/results/` for regression tracking.
- **Micro-benchmarks** (`benches/`, Criterion): mirror upstream's 13 `planetiler-benchmarks` classes, each landing with the component it measures (`BenchmarkTileCoord` → Phase 0, and so on).
- **Fairness rules** (see `parity/README.md`): pre-download inputs and never time the download; pin identical `--threads` / `-Xmx` / storage mode / tile-compression; discard a JVM warm-up run; report `mmap` and `ram` storage separately (mmap hides node-store bytes); compare decompressed payloads; assert byte-identity only once the pinned upstream build is confirmed deterministic, else assert semantic equality.
- **Pinned upstream:** Planetiler **v0.10.2** (`parity/fetch-upstream.sh`).

## Phase roadmap

- **Phase 0 — skeleton, spine & harness (current).** Module skeleton; `geom` + `model` (`SortKey`/`Value`/`Entry`/`PlanetilerConfig`) with tests; the time + peak-RSS harness and the first Criterion micro-bench; pinned upstream.
- **Phase 1 — output stack.** MVT encoder/decoder; `TileArchive` + MBTiles writer with per-tile gzip and `vector_layers` metadata. Deliverable: hand-built features → a `.mbtiles` that renders.
- **Phase 2 — thin end-to-end + real slicing.** GeoJSON source; `Profile` + `FeatureCollector`; `render` with hand-rolled clip, per-tile slicing, and polygon repair; in-memory sort. First head-to-head.
- **Phase 3 — pipeline machinery.** Worker pipeline + generic external merge sort + whole-tile dedup. Parallel and beyond-RAM.
- **Phase 4 — OSM, laddered.** (a) nodes/points, (b) node store + ways/lines, (c) relations + multipolygon assembly — each rung unlocking named upstream examples.
- **Phase 5 — scale & the full schema.** mmap `sparsearray` node store; PMTiles; Natural Earth + shapefile sources; the OpenMapTiles basemap profile. The marquee time-and-memory number.
- **Phase 6 (optional) — Overture geoparquet.**

## Build & test

Run from the repo root:

- Build: `cargo build` (release: `cargo build --release`)
- Test: `cargo test`
- Lint: `cargo clippy --all-targets -- -D warnings`
- Format: `cargo fmt` (check: `cargo fmt --check`)
- Micro-benchmarks: `cargo bench`
- Macro measurement: `cargo run --release --bin bench -- measure -- <program> <args…>`
