# Ways campaign: increment 1 falsified; the consume is allocator-bound

The probe (`ways-parallelism-probe.md`, #38) localized the pass-2 governor to the single-threaded consume (~87% of great-britain's pass-2 wall), split as `profile_s` ≈ 36 s (per-feature `process_feature`) + `render_s` ≈ 54 s (parallel render + serial sort feed). The named first increment was "parallelize `process_feature`." This is the record of building it, measuring it, and finding the assumption wrong — and of the lever the measurement actually exposed.

## Increment 1 — parallelize `process_feature`: FALSIFIED

Branch `engine/ways-parallel-process` (parked, not merged). It buffers source features into a chunk and runs `process_feature` across the rayon pool (`par_iter().map().collect()`, order preserved) instead of one feature at a time.

**Byte-identical — the full ladder is green** (branch vs `main`, tiles content hash + metadata hash + `tiles_shallow`/`tiles_data` row counts all match):

| config | verdict |
|---|---|
| monaco z12-14 | IDENTICAL |
| rhode-island z12-14 (default) | IDENTICAL |
| rhode-island z12-14 (`--sort-memory-mb 8`, spill path) | IDENTICAL |
| great-britain z12 sparse | IDENTICAL |

**But it is slower, not faster.** great-britain z12 sparse, interleaved best-of-2 on a contended box; `render_s` (unchanged code) landing identical across the paired runs is the control that the box was stable within each pair:

| term | main (serial) | branch (parallel) | Δ |
|---|---|---|---|
| `profile_s` | 35.67 s | 38.35 s | **+7.5% (worse)** |
| `render_s` | 54.14 s | 54.19 s | ~0 (control) |

**Why.** `--layer osm` builds `PassthroughProfile` (`cli.rs`), whose `process_feature` does `add_all_tags` → clone the source geometry + clone the tags `BTreeMap` per feature (`profile/collector.rs`). That term is **pure allocation, not compute.** Spreading allocation across the rayon pool fights the global allocator, and the parallel collect holds every inner `Vec` at once instead of one at a time — so it costs more, not less. The probe named the right term; the fix assumed the wrong bottleneck. `process_feature` here is allocator-bound.

## Root cause: the whole consume is allocator-bound

If the 36 s `profile_s` is allocation, so is much of the 54 s `render_s` (render clones geometry, builds blobs, allocates per piece) and the write stage (decode blobs, encode tiles). The default system `malloc` serializes all of it under multi-thread churn. That points the lever away from restructuring the pipeline and toward the allocator itself — a runtime-level change, not an algorithmic one.

## The fix: mimalloc as the global allocator

A one-line global-allocator swap to `mimalloc` (`src/main.rs`), nothing else. **Byte-identical** — allocation is memory management, not computation, so it cannot change output bytes, and the full ladder confirms it (mimalloc vs `main`, all IDENTICAL on tiles content hash + metadata hash + `tiles_shallow`/`tiles_data` row counts):

| config | verdict |
|---|---|
| monaco z12-14 | IDENTICAL |
| rhode-island z12-14 (default) | IDENTICAL |
| rhode-island z12-14 (`--sort-memory-mb 8`, spill) | IDENTICAL |
| great-britain z12 sparse (`cc370e3a1f152851`) | IDENTICAL |

great-britain z12 sparse, interleaved matched-load round (main and mimalloc back-to-back at load ≈ 4.8):

| term | main | mimalloc | Δ |
|---|---|---|---|
| `profile_s` | 36.54 s | 30.14 s | **−17.5%** |
| `render_s` | 54.17 s | 45.23 s | **−16.5%** |
| write stage | 45.56 s | 41.06 s | **−9.9%** |

Peak RSS (load-insensitive, `/usr/bin/time -l`, `RUST_LOG=warn` end-to-end run):

| | max RSS | peak footprint | wall |
|---|---|---|---|
| main | 4.77 GiB | 1.76 GiB | 166.3 s |
| mimalloc | **3.97 GiB (−17%)** | 1.80 GiB (+2%) | **146.2 s (−12%)** |

Every phase improves — the signature of an allocator-bound pipeline lifting at once. And on this workload mimalloc does **not** trade memory for speed: max RSS falls 17% and the anonymous footprint is flat. On the local extract it wins both clauses of the goal (faster + less memory), byte-identically.

## Caveats and follow-ups

- **Contended shared box, so the wall deltas are directional.** Absolute seconds are inflated by other load; the interleaved matched-load pairing and the within-run `render_s` control are what make the deltas trustworthy, and byte-identity + peak-RSS are load-insensitive. A quiet-box wall confirmation is a cheap next rung, not a blocker — the sign and rough magnitude are robust across every run.
- **Planet-scale RSS is the one thing still to verify.** These numbers are great-britain z12 on an M2 Max, not the planet. mimalloc's per-thread segment caching is unproven under the planet's sustained multi-hundred-GB churn, where RSS was the wall (`project-planet-run`). GB shows RSS *falling* 17%, but the planet is a different regime — the next planet re-run must watch RSS. A follow-up validation, not a reason to hold a byte-identical, dual-winning extract change.

## Configuration

- Box: Apple M2 Max, 12 cores (8P+4E), 96 GB, macOS 26.5.2, rustc 1.97.0. Contended (load 3–9) throughout; runs interleaved and paired for fairness.
- Input: `great-britain.osm.pbf` (2.0 GB), flags `--layer osm --minzoom 12 --maxzoom 12 --storage sparse --force` (defaults for min-size/simplify/buffer, so `render_s` differs from #38's planet-flag run; `profile_s` matches #38's ~35.9 s, as expected since it is render-flag-independent).

## Status

- **mimalloc: shipped to `main`.** Byte-identical on the full ladder, faster + lower RSS on every local scale — it passes the requirements it can be held to locally. The one open validation is planet-scale RSS (next planet run).
- `engine/ways-parallel-process` — parked receipt of the falsified increment (byte-identical, perf-negative). Not merged; kept as the record of why the campaign turned to the allocator, not more parallelism.
