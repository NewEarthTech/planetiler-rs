# Ways campaign: the local probe (before any fix)

The planet head-to-head licensed a pass-2 parallelism campaign: 74 minutes at ~12% CPU on 32 cores, with render starved behind it (`planet-headtohead.md`). The standing rule is probe first -- a cheap instrumented run on a local extract that reproduces the bottleneck shape and names the phase to fix, before a line of the fix is written. This is that probe. It reproduces the shape on great-britain and localizes the governor to the **single-threaded consume**, not the parallel decode.

## Instrumentation

Log-only, byte-identity-neutral (verified on the full ladder, see below):

- `for_each_block_parallel` (`src/source/osm.rs`) is bulk-synchronous -- serial read of 64 blobs, parallel decode burst, serial consume -- with no overlap. Split its wall three ways (`read_s` / `decode_s` / `consume_s`) per pass, so the idle silicon has a name.
- The consume is the pipeline `sink`, which does two very different things per feature. Split it (`src/pipeline.rs`): `profile_s` = per-feature `process_feature` + `FeatureCollector` (serial), `render_s` = `flush_render_batch` (parallel render across cores, then a serial `sort.add` feed).

## Configuration

- Box: Apple M2 Max, 12 cores (8P+4E), 96 GB, macOS 26.5.2, rustc 1.97.0. The box was **contended** by other work during the run (load 7-16), so absolute seconds are inflated; the phase *split* is the robust signal and it is consistent across two runs (pass-2 consume 68.4 s then 69.3 s, decode 9.4 s then 10.1 s).
- Input: `great-britain.osm.pbf` (2.0 GB), planet flags: `--layer osm --minzoom 12 --maxzoom 12 --storage sparse --min-size 0 --simplify 0 --buffer 0`.
- Raw log: `bench/results/ways-probe-20260723-gb.txt`.

## What the probe shows

Per-phase wall (second run; the first agrees within the noise):

| phase | wall | read | decode (parallel) | consume (serial) | consume share |
|---|---|---|---|---|---|
| pass 1 (nodes) | 24.0 s | 0.38 s | 5.21 s | 18.39 s | **77%** |
| pass 2 (ways) | 79.8 s | 0.37 s | 10.07 s | 69.34 s | **87%** |

And the consume, split across the whole stream (both passes + the ~21 s relation tail):

| consume term | wall | nature |
|---|---|---|
| `profile_s` (process_feature + collect) | 35.9 s | **serial, per-feature, embarrassingly parallel** |
| `render_s` (flush_render_batch) | 58.6 s | parallel render, then a **serial `sort.add` feed** |

For reference, the write stage (merge + encode + write) was 42.9 s for 21,324 tiles; the node store finished at 230 M nodes, avg run length 7.29, 482 MB index + 1.76 GB values (fits in RAM -- see the scale caveat).

**The shape reproduces and the governor is named.** Pass 2 spends 87% of its wall in a single-threaded consume while the parallel decode is 12% and 11 of 12 cores idle -- the local mirror of the planet's 74 min at 12%. The decode being cheap here (10 s) is the point: even with node lookups nearly free (GB's store is RAM-resident), pass 2 is still consume-bound, so the bottleneck is **structural serialization, not the node-lookup arithmetic**. The consume itself is ~38% serial per-feature profile work and ~62% render, and the render's own feed to the sort is serial too.

## The fix, and why it is byte-identity-safe

The consume is a single thread doing work that is independent per feature. The external sort makes this safe to parallelize: **its result is a function of the entry multiset, not the order entries are added** (equal keys carry identical bytes), so `process_feature` and the render feed can run in any order / on any thread and the output is unchanged by construction (still verified on the ladder before merge). This is the same order-independence the render-flush chunking already leans on.

Design, in order of payoff:

1. **Parallelize `process_feature`.** 35.9 s of pure serial per-feature work on one core. Fold it into the parallel producer (the decode closure already runs per-block across cores) so each worker emits render-ready `VectorFeature`s, or add a parallel map between decode and the sort feed. Reclaims most of it across the 8 perf cores.
2. **Overlap decode with consume.** The bulk-synchronous loop stalls decode during consume and vice-versa. A reader -> decode -> consume pipeline with bounded channels overlaps the ~10-20 s of decode with the consume and keeps the render continuously fed -- directly attacking the planet's "render starved behind pass 2," since render stops waiting on a serial feed.
3. **The serial `sort.add` feed** inside `flush_render_batch` is the remaining serial term inside `render_s`; size it with a follow-up split (render-collect vs sort.add) and parallelize the feed (sharded sort inputs merged later) if it proves to be the floor.

Super-linear payoff is expected exactly as the head-to-head predicted: feeding the render faster collapses its starved tail into overlap. The first increment is (1) -- the largest, cleanest, and most clearly order-safe.

## Scale caveat (what GB does *not* test)

GB's node store is RAM-resident, so this probe isolates the **structural** consume-serialization, which is scale-invariant and the right first lever. It does *not* reproduce the planet's cache-hostile node-lookup cost (496 M-run binary search, ~29 L3-missing probes per lookup) that inflates pass-2 *decode* at planet scale. That is a separate, additive planet-scale factor; the fix above addresses the governor either way, and a europe-scale re-probe (bigger store, ~50 GB) is the cheap next rung if decode's share needs confirming under a cache-hostile store before the planet.
