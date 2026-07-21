# Planet pre-flight: what a whole-planet run needs, measured

Planet is **93.9 GB** (`planet-260713`), **2.71× europe by bytes**. The largest prior input was europe (34.6 GB pbf, 625 M sorted entries, 739,799 tiles at z12).

This file exists because a planet run is a multi-hour, paid-for event where the interesting failures land *late* — after the read/render stage has already burned hours. The projections below were made before spending anything, per the rule the partitioned-merge experiment bought: probe first, build second. **The run has since happened — measured results are in the "First planet run" section at the top; the projections that follow are kept for the record.**

## First planet run (2026-07-20, r7gd.8xlarge · 32c/256 GB · us-west-2)

**Superseded in part (instrumented probe, 2026-07-20 eve): the "write stage" attribution in this section is wrong — the wall is the RENDER stage (`flush_render_batch`), confirmed as hypothesis H-A. See "Planet wall confirmed — H-A" below.** Runs C and D shipped no log home, so the write-stage reading was inference from RSS shape and mtimes; an instrumented run caught the mechanism directly. The node-store vindication, chunk-count measurement, and cost findings in this section all stand.

**Result: planetiler-rs does not complete a whole planet on a 256 GB box.** It builds cleanly through pass 1 and pass 2, then explodes in the write stage. Two prior attempts OOM-wedged the box (no memory guard); a third with an RSS watchdog caught it cleanly at 235 GB and preserved the trajectory. No Planetiler head-to-head yet — our runner only starts the Java side if ours completes.

**The node store — the term I feared most — beat its projection.**

```
sparse node store finished
  nodes = 10,742,674,832    runs = 496,166,544    avg_run_len = 21.65
  index (heap) = 7.4 GB     values (mmap) = 80 GB     pass 1 wall = 20.2 min
```

The projection argued europe's run-length (9.92) was a conservative floor because a geographic extract is a scattered subset of a global id space. Planet came in at **21.65** — denser still — so the heap index is **7.4 GB**, not the 41 GB the uniform-deletion worst case predicted. **The low-memory node-store architecture holds at 10.7 billion nodes.** RSS sat at ~100 GB, stable, through pass 1, pass 2, and sort-feed — well under the box.

**The wall is the write stage, and it is anonymous heap, not the node store.** With `--sort-memory-mb 1024` the sort spilled **297 chunks** (comfortably under the raised fd limit — that pre-flight concern was real and is handled). Sort-feed ran to ~00:48; the last chunk landed at 00:48:32; `/data/out` never received a single tile. Then:

```
RSS (10 s samples, final minute):  117 → 117 → 120 → 152 → 180 → 208 → 235 GB   (watchdog kill)
major faults across that climb:    685867 → 685867 → 685867 (flat)
```

~120 GB allocated in ~40 seconds with zero major-fault activity: pure anonymous heap, at the instant the sort finishes and the merge begins grouping entries into tiles. The write pipeline materializes whole decoded tiles to encode them, and at planet z12 — uncapped — that materialization runs away before the first tile is written. The scoreboard already documents the same shape at low zoom (an uncapped mega-tile: 59 GB → 5.9 GB with `--max-features-per-tile 50000`); planet reaches it at z12 from global density.

**Named fixes, in order of effort:**
- `--max-features-per-tile` — bounds features per tile-layer *before decode* (proven 10× at low zoom). Cheapest test of whether a capped planet completes on 256 GB; changes output, so not a like-for-like europe/GB comparison.
- Stream the write stage so whole tiles are never fully materialized — the principled fix, same move as the parallel-write and relation-capture rounds that took europe 67 → 53 GB. Makes an *uncapped* planet fit, which is the real target.
- A bigger box (r7gd.16xlarge, 512 GB) only buys headroom; if a single tile's materialization is unbounded it postpones the wall rather than removing it.

Cost of the finding: ~$15 across four dispatches (two wedges before the watchdog existed, a healthy run whose log went to /dev/null, and this one). Lessons folded back into the runner: RSS watchdog, unbuffered stdout capture, self-cleaning scratch.

## Planet wall confirmed — H-A, the render-flush working set (instrumented probe, 2026-07-20 eve)

The write-stage attribution above was the best reading available from runs C and D, but those runs shipped no planetiler-rs log home — every claim was inferred from RSS shape and file mtimes. Phase 0 added a `RenderProbe` and `WriteProbe` to the pipeline (#32); Phase 1 made the runner ship its log home every poll and byte-verify it before terminating (#33 — the fix for exactly that C/D failure). One instrumented planet run (~$5, r7gd.8xlarge, `main`) then caught the mechanism directly and **falsifies the write-stage theory**: the wall is the **render** stage, and not a single tile is ever written.

The site is `flush_render_batch` (`src/pipeline.rs`), which collects **every per-tile piece of a whole render batch into one unbounded `Vec`** before feeding the sort. For ~1h50m the largest such flush held a steady **15.7 MB**. Then a cluster of planet-scale multipolygons (ocean coastlines, ice shelves) arrived, each clipping into 10⁵–10⁶ z12 tiles carrying full osm-qa tags:

```
max_flush_bytes = 8.8 GB    max_flush_pieces = 1,145,294    from one feature (id 656343)
WARN large rendered working set:  656343 = 8.8 GB, 14241863 = 4.24 GB, 12211483 = 1.6 GB, +8 more
```

Several rendering concurrently across 32 rayon workers ran the anonymous heap away, logged at ~3 s resolution:

```
VmRSS 197 → 238 GB    anon 107 → 148 GB    (lockstep, ~2 GB/s)    major faults DEAD FLAT
→ VmRSS-backstop watchdog kill at 238 GB    ·    final_run=false    ·    0 tiles written
```

Anon peaked at 148 GB and never reached its 200 GB ceiling — VmRSS RAM-limited first, so the VmRSS backstop (235 GB) is what fired, catching it 3 GB over the line; the 2 s adaptive sampling near the ceiling kept the overshoot small where C/D overshot by 26 GB at the old 10 s cadence. The run reached the wall at ~111 min of pipeline time (C/D's inferred "~95–100 min" was a rough mtime guess). It died during `final_run=false`, so the monster features render inline during the stream, not only at a separate relation-only tail.

**The fix (superseding the "Named fixes" list above):** bound `flush_render_batch`'s working set — process each batch in feature-order sub-chunks under a rendered-bytes budget (~256 MB): par-render a sub-chunk, feed the sort, drop, repeat; size sub-chunks adaptively from summed input coords so the common path (small features) keeps today's full-batch parallelism and a single monster renders alone. This is **byte-identity-safe by construction**: the external sort's output is a function of the entry multiset (total order on key+value bytes; equal entries are indistinguishable), so re-chunking the feed cannot change the output — still verified on the standard set before any further cloud spend. `--max-features-per-tile` and "stream the write stage" both targeted a stage that never runs; H-B (the write-stage single-tile term) stays a real but latent liability, not the killer.

Evidence, checked in: `bench/results/planet-probe-20260720.stderr` (the full 85 KB probe log, byte-verified home) and `planet-probe-20260720-mem.log` (the 3 s memory trajectory). This was a diagnostic run, not a benchmark — mark it instrumented in any recorded numbers. Running cost across all planet dispatches is now ~$20.

## The two terms that scale badly

A code audit named five candidate failures at planet scale. Three are dismissible by configuration and are covered in the checklist at the bottom. Two needed numbers.

### 1. The sparse store's run index (the one genuinely unbounded term)

The sparse node store advertises ~8 bytes/node, but **that is the memory-mapped value file, not RSS.** Alongside it sits `runs: Vec<(i64, u64)>` on the heap — 16 bytes per *run*, where a new run starts at every gap in the id space. So the heap cost is set not by node count but by how holes are distributed through the ids, which no amount of reading the code will tell you.

The pessimistic reading is ugly. If deleted ids were scattered uniformly (planet holds ~9.5 B live ids out of ~13 B allocated, so ~27% gaps), mean run length would be ~3.7 and the index would want **41 GB of heap**, with a `Vec` doubling transient far above that. The only measured datapoint in the code's own docs was rhode-island at mean run 4.9, which sits right in that bad regime.

Measured, with the store now reporting its own shape:

| extract | nodes | runs | mean run length | index heap | value file |
|---|---:|---:|---:|---:|---:|
| great-britain (2.14 GB) | 230,257,811 | 31,601,155 | **7.29** | 482 MB | 1.76 GB |
| europe (34.6 GB) | 3,779,381,747 | 381,061,449 | **9.92** | 5.81 GB | 28.8 GB |

**Mean run length rises with extract size, and that resolves the question in our favour.** It is the expected direction and the reason matters: a geographic extract is a *scattered subset* of a global id space, so it is the pessimal case, and the bigger the extract the denser its coverage gets. rhode-island 4.9 → great-britain 7.29 → europe 9.92. The planet is the fully dense case and is the one file that cannot be a subset of anything, so its mean run length should be **at least** europe's 9.92 and plausibly well above it.

Projecting at 9.5 B nodes, holding europe's ratio as a conservative floor:

| mean run length | runs | index heap | value file (mmap) |
|---|---:|---:|---:|
| 9.92 (europe's, as a floor) | 958 M | **14.3 GB** | 71 GB |
| 12 | 792 M | 11.8 GB | 71 GB |
| 15 | 633 M | 9.4 GB | 71 GB |

**So the run index costs roughly 10-15 GB at planet, not 41.** The uniform-deletion worst case is falsified by the trend in the measurements. This is comfortable on any box we would rent, and it is now logged at `finish` so a run says which regime it is in within the first pass instead of dying silently in the third hour.

### 2. The merge's chunk count against the file-descriptor limit

The k-way merge opens **every** spilled chunk at once and holds them for the whole merge. Chunk count is therefore a hard floor on file descriptors, and the open happens *after* the entire read/render/sort-feed stage — the most expensive possible place to fail.

Measured on great-britain: **33 chunks for 44,195,923 entries** at the default 256 MB sort buffer, i.e. ~1.34 M entries per chunk.

| input | entries | chunks @ 256 MB | chunks @ 1024 MB |
|---|---:|---:|---:|
| great-britain | 44 M | 33 (measured) | — |
| europe | 625 M | ~467 | — |
| **planet** | ~1.6 B | **~1,195** | **~299** |

**Planet at the default buffer lands above a stock `ulimit -n` of 1024.** Not marginally — and europe at ~467 was already closer to the wall than anyone knew. Two independent fixes, and the run should carry both: raise `--sort-memory-mb` to 1024, which cuts chunks ~4× and speeds the merge anyway, and raise `ulimit -n` so the descriptor ceiling is nowhere near the working range. The merge now logs its chunk count up front and warns past 900.

## Sizing

Derived from the measurements above plus the on-disk entry format (8-byte key + 4-byte length + value; great-britain averages ~164 bytes/entry on disk).

| | europe (measured) | planet (projected) |
|---|---:|---:|
| input pbf | 34.6 GB | **93.9 GB** |
| nodes | 3.78 B | ~9.5 B |
| sort entries | 625 M | ~1.6 B |
| node store value file | 28.8 GB | ~71 GB |
| node store index (heap) | 5.81 GB | ~14 GB |
| sort spill | ~70 GB | **~265 GB** |
| output archive (z12) | 24 GB | ~60 GB |
| peak scratch | ~100 GB | **~350 GB** |
| peak RSS | 47-53 GB | **~130-140 GB** |

Most of that RSS is clean, evictable mmap pages of the value file, so the kernel can drop it under pressure rather than OOM — the architectural point the sparse store exists to make. The number to actually watch is the ~14 GB index plus the between-pass relation state, which is anonymous heap and cannot be evicted.

Two lifetime notes that keep the peak lower than a naive sum: the node store and way-geometry store are dropped when the OSM stream returns, *before* the merge/write stage runs, so the 350 GB peak is not the sum of every line. And europe's measured scratch at the end of pass 1 was 50 GB, consistent with the 28.8 GB value file plus partial spill.

## Pre-flight checklist

1. **`--storage sparse` is mandatory.** The default is `ram`, whose store is `Vec<(i64, u64)>` at 16 bytes/node: **152 GB** at planet, and because it grows by doubling, ~228 GB live during the final reallocation. Nothing in the code enforces or warns about this.
2. **Do not use a `.pmtiles` output path.** The PMTiles writer buffers the entire archive into one `Vec` at `finish`; the archive format is chosen purely by file extension, so this is one typo from a 60+ GB allocation.
3. **`--sort-memory-mb 1024` and `ulimit -n 65536`.** See the chunk-count section; either alone is probably enough, both is cheap.
4. **`TMPDIR` on the instance-store NVMe, ≥600 GB free.** All four scratch consumers use `tempfile` and honour it; unset, ~350 GB tries to land on the root volume.
5. **z12, matching europe and great-britain.** Not just for comparability: at z14 the MBTiles dedup map (`HashMap<u128, i64>`, never bounded) reaches ~17.7 GB steady and ~26.5 GB during resize. At z12 it is negligible. A z0-14 planet build is a different and much more expensive question.
6. **NVMe, not gp3.** The europe cloud run was disk-bound on gp3: 2601 s in stream/render/sort-feed against ~1410 s for the same stage on laptop NVMe.

## Known gaps, stated plainly

- **Planet's mean run length is projected, not measured.** The argument for ≥9.92 is structural (a dense global file cannot be sparser in its own id space than a subset of it) and the trend across three extracts supports it, but the planet is the only file that settles it. The instrumentation reports it in pass 1, so a run that disagrees is visible early and cheap to abort.
- **Entry count is projected** from a nodes-to-entries ratio that held across great-britain and europe. Planet has proportionally more low-density area, which if anything pushes entries down.
- **No crash recovery.** The MBTiles writer runs `journal_mode = OFF` inside a single archive-wide transaction, so any late failure loses the whole run rather than part of it.
