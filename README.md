# planetiler-rs

A Rust port of [Planetiler](https://github.com/onthegomap/planetiler), the Java tool that turns OpenStreetMap extracts into Mapbox Vector Tile archives. Same job: read an `.osm.pbf`, write an `.mbtiles` you can serve to a map.

Why port a thing that already works? Memory. Planetiler is fast, but it buys that speed with RAM: it wants a big heap, and the heap grows with the input. The bet here is that a streaming pipeline with disk-backed sorting and an off-heap node store can produce the same tiles in a fraction of the memory, and be quick enough that you don't mind. That's a testable claim, so we test it. Constantly.

## The numbers

Same task (all OSM features into one `osm` layer, à la Planetiler's `osm-qa`), same machine (12 cores, 96 GB), same inputs, pinned upstream v0.10.2. Tile counts agree to within 1% at every size (monaco: exact).

| extract | planetiler-rs | Planetiler | speed | memory |
|---------|---------------|------------|-------|--------|
| monaco (0.7 MB) | 0.41 s / 84 MB | 2.20 s / 1068 MB | ~5× faster | ~13× less |
| rhode-island (49 MB) | 4.0 s / 1.1 GB | 8.94 s / 7.8 GB | ~2.2× faster | ~7× less |
| great-britain (2.0 GB) | 153 s / 5.8 GB | 81 s / 29.1 GB | 1.9× slower | ~5× less |

Yes, we lose one. Great Britain is right there in the table: Planetiler's mature parallel pipeline out-runs us at country scale, and pretending otherwise would defeat the entire point of keeping benchmarks. The gap was 4.1× before profiling; it's 1.9× now; the work continues. Full history (including the experiments that made things worse and got reverted) lives in [`bench/results/`](bench/results/).

## Output parity, honestly stated

"Same tiles" is doing a lot of work in a claim like this, so here is exactly where it stands, measured on monaco against upstream:

- **Tile set: exact.** Same 102 tiles, byte-for-byte the same coordinates. Bounds clipping and relation assembly closed this.
- **Attributes: byte-identical.** Including the `osm-qa` metadata (`@id`, `@type`, `@version`, `@timestamp`).
- **Per-tile geometry: 51 of 102 tiles semantically identical.** The rest differ by sub-pixel simplification choices (our Douglas-Peucker keeps slightly different vertices than theirs). We rendered overlays of ours vs theirs to check: you cannot see the difference. But the oracle can, so it stays on the list.

This is not yet a drop-in replacement for real Planetiler profiles (OpenMapTiles, custom Java profiles, PMTiles output). Today it builds osm-qa-style single-layer archives. If you need the full schema, use Planetiler; it's excellent software and this project would not exist without it.

## Quick start

You need Rust 1.85+ (edition 2024) and `protoc` on your PATH (the build script compiles the MVT proto).

```sh
cargo build --release

# OSM extract in, MBTiles out
./target/release/planetiler-rs generate \
  --source monaco.osm.pbf --output monaco.mbtiles \
  --layer osm --minzoom 12 --maxzoom 14 --force
```

GeoJSON sources work too (`--source features.geojson`). The flags you'll actually reach for:

- `--storage ram|mmap|sparse` — node-location store. `ram` is the fast default. `sparse` streams a run-compressed store to disk and memory-maps it: the planet-scale path, and the one in the table above.
- `--bounds w,s,e,n` — clip tile output. Defaults to the pbf's header bbox (like Planetiler); `--bounds world` disables.
- `--max-features-per-tile N` — cap features per tile layer. `0` (default) keeps everything; at country scale with low zooms, an uncapped z0 tile will eat your RAM for breakfast (we measured 59 GB; capped, 5.9 GB).
- `--sort-memory-mb` — external-sort budget before spilling to disk (default 256). We measured 256 MB vs 16 GB: same wall time. Spill I/O is cheap; don't spend RAM here expecting speed.
- `--min-size`, `--simplify`, `--buffer` — per-zoom feature filtering, Douglas-Peucker tolerance (px), tile buffer.

## The harness

Two binaries: one makes tiles, one keeps us honest.

```sh
# wall time + peak RSS for any command (ours or the Java jar)
cargo run --release --bin bench -- measure -- <program> <args...>

# semantic diff of two archives: tile-set overlap, per-tile equality, feature counts
cargo run --release --bin bench -- compare ours.mbtiles theirs.mbtiles

# render a tile to SVG; two archives overlay (blue fill vs red outline) so divergence is visible
cargo run --release --bin bench -- render 12 2132 1493 out.svg ours.mbtiles theirs.mbtiles
```

`parity/fetch-upstream.sh` downloads the pinned Planetiler jar so every head-to-head runs against the same reference. The rule this repo lives by: no capability is done without a number attached, and the number goes in the repo even when it's embarrassing.

## Design, briefly

Streaming source → profile → render (project, slice into covering tiles, clip, quantize) → external merge sort keyed by tile → group → MVT encode + gzip → MBTiles. Peak memory is bounded by one render batch, the sort buffer, the node store you chose, and one decode window: never the whole input, never all the tiles. PBF blocks decode in parallel windows with ordered consumption, so output is deterministic down to the byte across every storage mode and thread count. `docs/ARCHITECTURE.md` has the long version.
