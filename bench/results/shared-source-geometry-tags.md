# Ways campaign: sharing the source geometry and tags instead of deep-cloning them

`consume-alloc-avoidance.md` (#41) closed by naming what was left in the consume: the per-source-feature `geometry` and `tags` deep clones in `FeatureCollector::add` / `add_all_tags`. Those were the last big allocation sites on the serial path — a way with five tags cost a coordinate-vector copy plus a `BTreeMap` node plus a `String` per key and per string value, every feature, 43.3 M times on great-britain.

The note guessed this would cost API surface, because moving out of the source instead of cloning would force the profile to reach the source *through* the collector. It doesn't have to. Sharing gets the same result and leaves `process_feature(&source, &mut collector)` exactly as it was.

## The change

`SourceFeature::geometry` and `::tags` become `Arc<…>`, and `VectorFeature` carries the same `Arc`s. Emitting a feature is then a refcount bump instead of a deep copy.

- `add_all_tags` — `Arc::clone` of the source's tags. Was: a full tag-map deep clone.
- `add` — `Arc::clone` of one shared empty map (`NO_ATTRS`), so the no-tags path allocates nothing at all.
- `attr` — `Arc::make_mut`, i.e. **copy-on-write**. A profile that only reads shared tags never pays for a copy; one that writes pays exactly once. `attr_copies_on_write_instead_of_mutating_shared_tags` pins the semantics: a write through one emitted feature reaches neither its sibling nor the source.

Deref makes the sharing invisible at every read site (`source.tags.get`, `extract(&feature.geometry)`, `predict_pieces`), so the change is confined to the type declarations, the collector, and the source readers' construction sites.

## Byte-identity

| config | verdict |
|---|---|
| monaco z12-14 | IDENTICAL |
| rhode-island z12-14 (default) | IDENTICAL |
| rhode-island z12-14 (`--sort-memory-mb 8`, spill) | IDENTICAL |
| great-britain z12 sparse | IDENTICAL |

(tiles content hash + metadata hash + `tiles_shallow`/`tiles_data` row counts, branch vs `main` at #41.)

## Measurement

great-britain z12 sparse, planet flags, **four interleaved pairs — two run main-first and two branch-first**, because the first A/B had the branch always running second into a box still hot from main's run. `decode_s` is the control (untouched code; within 1.3% across all eight runs).

| term | main best | branch best | Δ best | main mean | branch mean | Δ mean |
|---|---|---|---|---|---|---|
| `profile_s` | 22.63 s | 15.20 s | **−32.8%** | 24.94 s | 15.60 s | −37.4% |
| `render_s` | 40.77 s | 46.86 s | **+14.9%** | 41.84 s | 48.49 s | +15.9% |
| pass 1 `consume_s` | 11.47 s | 9.93 s | **−13.4%** | 12.27 s | 10.56 s | −13.9% |
| pass 2 `consume_s` | 45.13 s | 38.99 s | **−13.6%** | 48.78 s | 40.82 s | −16.3% |
| stream + sort-feed | 87.80 s | 80.23 s | **−8.6%** | 92.57 s | 82.81 s | −10.5% |
| write stage (untouched) | 36.32 s | 36.42 s | +0.3% | 36.69 s | 37.22 s | +1.4% |
| peak RSS | 3.777 GiB | 3.775 GiB | −0.1% | 3.927 GiB | 3.836 GiB | −2.3% |

The ordering worry was real and it was biasing *against* the branch: main-first pairs give branch 83.99 s vs main 90.53 s, branch-first pairs give branch 81.63 s vs main 94.62 s. The win survives both orders; the first A/B understated it.

**Peak RSS is a wash.** Best-of-4 is dead level and the ranges overlap (main 3.78–3.99 GiB, branch 3.78–3.97 GiB). An earlier two-pair read looked like a consistent −4.7% win; pairs 3 and 4 falsified that, which is exactly why the extra pairs were run. No memory claim in either direction.

### Where the time actually went

`render_s` rising while `profile_s` falls by twice as much is the interesting part, and the split says the saving is not where the probe's names suggest:

| pair | inside the sink (`profile_s`+`render_s`) | outside it (`stream` − both) |
|---|---|---|
| 1 | +1.58 s | **−5.36 s** |
| 2 | −2.41 s | **−6.89 s** |
| 3 | −6.75 s | **−8.78 s** |
| 4 | −3.18 s | **−7.27 s** |

The consistent, larger win is **outside the sink entirely** — in the OSM decode-and-collect path, which the consume split never instrumented. The reason is that `SourceFeature` shrank from **112 bytes to 48** (`Geometry<f64>` is a 56-byte enum and `BTreeMap` 24; both become an 8-byte pointer). Pass 2 materializes 43.3 M of these into per-block `Vec`s on the decode workers before the serial consume drains them, so more than halving the struct halves that buffering traffic. That is a bulk-memory win the campaign had not been looking for.

Inside the sink, the profile→render shift is a re-attribution, not a regression: the deep clone used to *read* the source geometry and *write* a fresh copy during `profile_s`, pulling the coordinates into cache right before the render walked them. Removing the copy also removes that accidental prefetch, so the render now takes the misses the clone was absorbing. Copying is more expensive than the misses it hid — pass 2's consume as a whole falls 13.6% — but the cost moves across the probe's boundary, and neither term alone tells the truth. Worth remembering the next time a `profile_s`/`render_s` delta is read as a verdict.

## Configuration

- Box: `net-mbp-johncarmack`, Apple M2 Max, 12 cores (8P+4E), 96 GB, macOS 26.5.2, rustc 1.97.0. Local only — nothing dispatched to cloud. Load 3–8 throughout, self-inflicted by the runs themselves; pairs interleaved in both orders, `decode_s` as control.
- Input: `great-britain.osm.pbf` (2.0 GB), `--layer osm --minzoom 12 --maxzoom 12 --storage sparse --min-size 0 --simplify 0 --buffer 0`. Ladder great-britain leg uses the standard gate config (defaults for min-size/simplify/buffer).

## Status

- **Shipped to `main`.** Byte-identical on the full ladder, −13.6% on the pass-2 consume and −8.6% on the whole stream stage, peak RSS unchanged, gates green (fmt, clippy, 124 tests).
- Remaining named levers in the consume, unchanged in order: overlap decode with consume (bulk-synchronous today — pass 2 stalls decode during consume and vice-versa, worth ~7.7 s of pass-2 decode), then the serial `sort.add` feed inside `render_s`. A cheap re-test of parallel `process_feature` is also now differently motivated: the falsified attempt (`engine/ways-parallel-process-PARKED-FALSIFIED`) was fighting allocation that this change has largely removed.
