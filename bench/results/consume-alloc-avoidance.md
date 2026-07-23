# Ways campaign: re-baselining pass 2 on mimalloc, then making allocation rarer

`ways-allocator-finding.md` ended on a conclusion rather than a fix: the pass-2 consume is allocator-bound, and mimalloc (#40) made every allocation cheaper. The obvious follow-on is that cheaper-than-faster-malloc is **no malloc** — so this is (1) an honest re-baseline of the pass-2 split against the mimalloc main, since every number in the #38 probe predates it, and (2) the first allocation-*avoidance* increment measured against that denominator.

## 1. Re-baseline: what mimalloc did to the pass-2 split

great-britain z12 sparse, #38's exact planet flags (`--layer osm --minzoom 12 --maxzoom 12 --storage sparse --min-size 0 --simplify 0 --buffer 0`), two runs, the quieter reported (box load 2.2 at start — the quietest this campaign has had).

| phase | wall | read | decode (parallel) | consume (serial) | consume share |
|---|---|---|---|---|---|
| pass 1 (nodes) | 17.62 s | 0.41 s | 3.97 s | 13.19 s | **75%** |
| pass 2 (ways) | 59.30 s | 0.36 s | 7.79 s | 51.15 s | **86%** |

Consume split across the whole stream: `profile_s` 27.22 s, `render_s` 42.18 s. Stream + sort-feed 95.44 s for 43,348,118 source features → 44,195,923 sort entries; write stage 36.44 s for 21,324 tiles.

Against #38's pre-mimalloc run of the identical config:

| term | #38 (system malloc) | now (mimalloc) | Δ |
|---|---|---|---|
| pass 1 wall | 24.0 s | 17.62 s | −27% |
| pass 2 wall | 79.8 s | 59.30 s | −26% |
| pass 2 decode | 10.07 s | 7.79 s | −23% |
| pass 2 consume | 69.34 s | 51.15 s | −26% |
| `profile_s` | 35.9 s | 27.22 s | −24% |
| `render_s` | 58.6 s | 42.18 s | −28% |
| write stage | 42.9 s | 36.44 s | −15% |

**Every term fell by about a quarter and the shape did not move** — the consume is still 86% of pass 2 (was 87%), decode still ~13%. That is the allocator-bound diagnosis confirming itself from the other side: a pure allocator swap lifted the whole pipeline uniformly without touching the governor. Making allocation *cheaper* does not change who is on the critical path; making it *rarer* is the next lever, and the consume is still where it lives.

## 2. Increment: allocation avoidance in the consume path

Three sites, all of them copies whose entire life is the next statement.

**(a) Per-piece attribute and layer clones — the big one.** `RenderedFeature` owned `layer: String` and `attrs: BTreeMap<String, Value>`, so `PreparedZoom::piece` did a `String` allocation plus a full `BTreeMap` deep clone (a node allocation, plus one `String` per key and per string value) **for every covering tile of every feature** — and then the very next statement serialized them into the feature blob and dropped them. `RenderedFeature<'a>` now borrows both from the source `VectorFeature`, which outlives every piece rendered from it. Only the clipped geometry is genuinely per-tile and stays owned.

**(b) Per-source-feature collector `Vec`.** `FeatureCollector` owned a `Vec<VectorFeature>` that the pipeline then drained into the render batch — an allocate/free pair plus a move of every emitted feature, per source feature, for a buffer that almost always held exactly one element. The collector now appends straight into the render batch and reports where this source feature's output starts.

**(c) Always-wasted key clones in `record_layer_stats`.** `BTreeMap::entry` takes its key by value, so `entry(key.clone())` allocated a `String` for the layer name and for every attribute key of every feature — and past the first handful of features every one of those keys is already present, so the allocation was pure waste. Now it looks up first and clones only a genuinely new key.

### Byte-identity

Encoding from borrowed fields is only safe if it produces the same bytes, so that is asserted directly rather than assumed: postcard writes a struct as its fields' encodings concatenated (no names, no field count) and serde's `impl Serialize for &T` forwards to `T`, so `FeatureBlobRef` and `FeatureBlob` emit identical bytes. `blob_ref_encodes_identically_to_owned` pins it down across every `mvt::Geometry` variant, all four `Value` shapes, and empty attrs, and round-trips the borrowed encoding back through the owned decoder. Nothing else in the increment touches what is emitted or in what order.

The ladder, branch vs `main` (tiles content hash + metadata hash + `tiles_shallow`/`tiles_data` row counts):

| config | verdict |
|---|---|
| monaco z12-14 | IDENTICAL |
| rhode-island z12-14 (default) | IDENTICAL |
| rhode-island z12-14 (`--sort-memory-mb 8`, spill) | IDENTICAL |
| great-britain z12 sparse | IDENTICAL |

### Measurement

Interleaved matched-load pairs, main and branch back-to-back, planet flags as above. `render_s` is no longer available as the within-pair control (this increment changes it), so **`decode_s` is the control** — untouched code, and it lands within 1–3% across all four runs, which is what makes the pairs comparable.

| term | main p1 | branch p1 | main p2 | branch p2 | best-of-2 Δ |
|---|---|---|---|---|---|
| pass 1 `decode_s` (control) | 4.01 s | 3.96 s | 3.95 s | 3.92 s | −0.9% |
| pass 2 `decode_s` (control) | 7.83 s | 7.75 s | 7.84 s | 7.62 s | −2.7% |
| pass 1 `consume_s` | 13.54 s | 12.37 s | 14.53 s | 12.34 s | **−8.8%** |
| pass 2 `consume_s` | 52.01 s | 48.41 s | 56.11 s | 49.00 s | **−6.9%** |
| `profile_s` | 27.22 s | 25.21 s | 30.27 s | 25.58 s | **−7.4%** |
| `render_s` | 44.37 s | 41.39 s | 44.03 s | 41.39 s | **−6.0%** |
| stream + sort-feed | 97.58 s | 92.37 s | 102.19 s | 92.75 s | **−5.3%** |
| write stage (untouched) | 36.73 s | 37.43 s | 36.46 s | 36.48 s | +0.1% |

The branch is also markedly more *reproducible* than main across the two pairs (`render_s` 41.389 s twice to the millisecond; `profile_s` within 1.5%, against main's 11%) — what you would expect from taking pressure off a contended global allocator.

**Peak RSS is a wash, not a win.** `/usr/bin/time -l` on the same runs: main 3.80 / 3.95 GiB, branch 4.01 / 3.94 GiB. The arms' ranges overlap and main's own two runs differ by 4%, so there is no supportable memory claim here in either direction. That is expected for this config: great-britain's peak is dominated by the sparse node store (482 MB index + 1.76 GB values) and the sort buffer, not by rendered pieces.

### What great-britain z12 understates

This config renders 44.20 M pieces from 43.35 M features — **1.02 pieces per feature**, because it is a single zoom. Site (a) saves one attribute-map deep clone *per piece*, so at one zoom it is worth about one clone per feature; across a real z0–14 archive a feature covers many more tiles and the saving scales with the zoom range. The same term is a working-set reduction, not just a time saving: every piece in a render flush used to carry its own attribute copy, which is exactly the resident-output term slab-mode and the piece chunker were built to bound (`render-flush-predictor.md`) — great-britain alone already has a feature predicted at 143,100 pieces. Both effects are largest at planet scale, which this box cannot measure; the next planet run gets them for free alongside the open mimalloc RSS question.

## Configuration

- Box: Apple M2 Max, 12 cores (8P+4E), 96 GB, macOS 26.5.2, rustc 1.97.0. Load 2–7 across the session; runs interleaved and paired, with `decode_s` as the within-pair control. Byte-identity and the piece/feature counts are load-insensitive.
- Input: `great-britain.osm.pbf` (2.0 GB). Ladder great-britain leg uses the standard gate config (z12 sparse, default min-size/simplify/buffer); the timed runs use the planet flags above, matching #38.

## Status

- **Shipped to `main`.** Byte-identical on the full ladder, ~7% off the consume on every term, peak RSS unchanged, gates green (fmt, clippy, 123 tests).
- Still standing in the consume, in payoff order: the per-source-feature `geometry` + `tags` deep clones in `FeatureCollector::add` / `add_all_tags` (these need the collector to own the source, so they cost API surface — a separate increment), then overlapping decode with consume, then the serial `sort.add` feed.
