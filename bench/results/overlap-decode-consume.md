# Ways campaign: overlapping decode with consume — built, byte-identical, and falsified

The #38 probe listed three fixes for the pass-2 governor. Two have landed (#41, #42). This is the third: `for_each_block_parallel` is bulk-synchronous — a serial blob read, a parallel decode burst, then a serial consume, with no overlap — so pass 2's ~7.7 s of decode is simply *added* to a ~39 s consume it could in principle hide under. That framing predicted a ~8-10% win. It did not survive measurement.

## What was built

A producer thread reads and decodes windows while the calling thread consumes the previous one, handing windows over a **rendezvous channel** (`sync_channel(0)`). Ordering is untouched: windows are produced in file order down one channel and drained in order, so `consume` sees exactly the sequence it saw before.

Memory was held neutral deliberately. A rendezvous keeps one decoded window in flight beyond the one being consumed, so the window was halved (64 → 32 blobs) to pay for it — resident decoded output stays the same 64 blobs the old loop held. The planet's wall was RSS, so an overlap that bought throughput with footprint would have been the wrong trade even if it had worked.

**It is correct.** Full ladder vs `main` at #42, all IDENTICAL (tiles content hash + metadata hash + `tiles_shallow`/`tiles_data` counts): monaco z12-14, rhode-island z12-14 default, rhode-island z12-14 spill, great-britain z12 sparse. Gates green, 124 tests.

## The measurement

great-britain z12 sparse, planet flags, four interleaved pairs in both orders (two main-first, two branch-first).

| term | main best | branch best | Δ best | main mean | branch mean | Δ mean |
|---|---|---|---|---|---|---|
| **stream + sort-feed** | **80.25 s** | **80.08 s** | **−0.2%** | **82.36 s** | **80.38 s** | **−2.4%** |
| pass 1 `decode_s` | 3.88 s | 4.38 s | +13.0% | 3.94 s | 4.46 s | +13.1% |
| pass 1 `consume_s` | 10.22 s | 11.57 s | +13.1% | 10.60 s | 11.70 s | +10.4% |
| pass 2 `decode_s` | 7.60 s | 8.08 s | +6.3% | 7.75 s | 8.20 s | +5.8% |
| pass 2 `consume_s` | 39.26 s | 46.10 s | +17.4% | 40.30 s | 46.38 s | +15.1% |
| write stage (untouched) | 36.20 s | 36.14 s | −0.2% | 36.75 s | 36.32 s | −1.2% |
| peak RSS | 3.787 GiB | 3.854 GiB | +1.8% | 3.917 GiB | 3.982 GiB | +1.7% |

(The phase terms no longer sum to the pass wall on the branch — that is the whole point of the change — so only `stream` is a like-for-like total.)

## Why it failed, which is the useful part

`consume_s` rose 17% and `decode_s` rose 6% while the total barely moved. The decode work **did not find idle silicon; it took cores from the consume's own parallel render.** Total CPU work is conserved, so overlapping two phases that already compete for the same 12 cores just re-labels where the time is spent.

The ~7.7 s ceiling assumed the cores were idle during the consume. That assumption came from the planet run (pass 2 at ~12% CPU on 32 cores) and from #38's probe, taken *before* #41 and #42. It is no longer true of this workload on this box: the consume's `render_s` uses the pool well, and `profile_s` — the genuinely single-threaded stretch that decode could have hidden under — was cut by a third in #42. **The two landed increments removed the very idleness this one needed.**

Peak RSS came out 1.8% higher rather than neutral. That is inside the ±4% run-to-run band established in `shared-source-geometry-tags.md`, so it is not a claim, but it is not the improvement the halved window was meant to guarantee either.

## What is worth keeping

One real property did show up: the branch is markedly **more stable**. Stream spread across four runs was 0.77 s versus main's 4.16 s; pass 2 `consume_s` spread 0.68 s versus 2.24 s. Overlapping absorbs the jitter that the bulk-synchronous loop passes straight through. That is a nice quality, but it is not the goal, and it does not pay for a producer thread, a channel, and a `consume_s` number that no longer means what it used to.

**The planet-shaped case remains untested and is not settled by this.** The planet measured pass 2 at ~12% CPU on 32 cores — genuinely idle silicon, a regime great-britain on 12 cores does not reproduce, and one where the node-lookup-heavy decode is far more expensive. The design is proven byte-identity-safe and its memory bound is by construction, so it is revivable if a planet-scale probe ever shows the idle cores again. It should not be revived on theory alone; that is what produced this result.

## Configuration

- Box: `net-mbp-johncarmack`, Apple M2 Max, 12 cores (8P+4E), 96 GB, macOS 26.5.2, rustc 1.97.0. Local only. Four pairs, both orders.
- Input: `great-britain.osm.pbf` (2.0 GB), `--layer osm --minzoom 12 --maxzoom 12 --storage sparse --min-size 0 --simplify 0 --buffer 0`.

## Status

- **NOT merged.** Branch `perf/overlap-decode-consume` is parked as the receipt. Byte-identical and correct; it simply does not buy anything here.

### The parked receipts, in one place

Three branches carry falsified experiments. All are byte-identical to their baseline, all are pushed to `origin`, and **none of them should ever be merged or deleted** — each is a reproducible negative result, so a future measurement that changes the premise can rebase and re-measure instead of rebuilding from prose.

| branch | experiment | verdict |
|---|---|---|
| `perf/parallel-merge` | partitioned k-way merge | +3.5% wall (great-britain), +0.4% (europe) |
| `engine/ways-parallel-process-PARKED-FALSIFIED` | parallel `process_feature` | +7.5% `profile_s` |
| `perf/overlap-decode-consume` | overlap decode with consume | −0.2%, inside the noise |

The verbose suffix on the middle one is deliberate: it was the only receipt that had never been pushed, so it got the loud name when it was backed up. "Falsified" rather than "failed" — the code works, the hypothesis did not.
- Remaining named lever in the consume: the serial `sort.add` feed inside `render_s`. Given this result, it deserves a split measurement (render-collect vs `sort.add`) proving the feed is actually the floor **before** any code is written — the probe-first rule, which this increment honoured on #38's evidence and still landed on a stale premise. Evidence has a shelf life; re-probe after the pipeline changes underneath it.
