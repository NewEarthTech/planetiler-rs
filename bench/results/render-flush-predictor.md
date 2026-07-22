# Render-flush predictor: name and measure the monster before it expands

Instrumentation-only increment opening the H-A fix. The planet probe confirmed the wall is `flush_render_batch`'s unbounded per-batch `Vec` — feature 656343 expanded into **8.80 GB / 1,145,294 z12 pieces in one flush** (`bench/results/planet-probe-20260720.stderr`). The prescribed fix bounds that working set with four order-preserving mechanisms, and all of them stand on one primitive: a per-feature *predicted-pieces* number computed **before** the render. The first predictor tried — summed input coords — was **falsified** by the probe (its pre-render warning fired zero times across the whole planet run while the multi-GB flushes happened). This increment builds the replacement, proves it changes no output, and measures whether it tracks reality — before any bounding code is written, per the repo's probe-first rule.

## What shipped (commit `3053878`)

- `render::predict_pieces(feature, opts)` — the summed covered-tile-rect product across the feature's zoom range, taken from the raw lng/lat bounding box. It is the same quantity `render_at_zoom` computes as `tile_count`, but derivable ahead of the render. The over-prediction invariant (`predicted >= emitted`, because simplify / min-size / bounds-clip can each only shrink the rect) is proven in `render::tests`.
- The pre-render warning keyed on `total_input_coords > 10M` (falsified) is replaced by one keyed on a feature's **predicted output pieces**, and it names the predicted *output* monster rather than the largest-input feature.
- `RenderProbe` now carries **predicted vs realized** pieces — per oversized flush and in the 30 s running summary — so a run leaves a fidelity trace.
- `WriteProbe::maybe_log`'s per-entry `Instant::elapsed()` is masked behind a 64 Ki entry stride (owed carryover; byte-identity-neutral, but it removes tens of seconds of syscalls from the serial merge thread at planet scale).

Instrumentation only: nothing rendered, sorted, or written changes.

## Byte-identity vs `main` (the instrument changes nothing)

Standard recipe — sqlite `tiles` content hash, `metadata` hash, and `tiles_shallow` / `tiles_data` row counts — each config run on both the `main` and branch release binaries:

| config | tiles hash | shallow / data | verdict |
|---|---|---|---|
| monaco z12-14 | `89797462…` | 102 / 97 | identical |
| rhode-island z12-14 (default) | `4ced9f38…` | 2217 / 2142 | identical |
| rhode-island z12-14 (`--sort-memory-mb 8`, spill path) | `4ced9f38…` | 2217 / 2142 | identical |
| great-britain z12 sparse (planet flags) | `15512fb5…` | 21324 / 18544 | identical |

The great-britain z12 sparse check pays the byte-identity deferral disclosed in the Phase 0 (#32) merge.

## Fidelity: does the tile-rect predictor track reality? (great-britain z12 sparse, planet flags)

The country-scale run is the planet mechanism in miniature. The Phase 0 smoke recorded its largest flush at 77 MB / 40,154 pieces, and this run reproduces it exactly: final summary `max_flush_bytes=77498660`, `max_flush_pieces=40154`.

**Aggregate: predicted 45,690,807 vs actual 44,195,923 pieces = 1.034× (3.4% over).** Because the per-feature invariant makes every term over-count, this whole-run ratio is the tightest honest statement of the predictor's quality — and it is tight. That is the property the fix needs: predicted-pieces chunking will engage only when the realized flush warrants it, so the common path stays a single chunk (today's exact code path, today's full parallelism).

**Attribution fixed, demonstrated.** The one pre-render warning that fired:

```
WARN render batch: feature predicted to expand into a huge tile-piece set
  features=4096  max_predicted_id=784440712  max_predicted_pieces=143100
  largest_input_id=785308422  largest_input_coords=1439  total_input_coords=51595
```

The predictor names feature **784440712** — 143,100 predicted pieces — as the monster. The old input-coords attribution would have named **785308422**, a 1,439-coord trivial feature, and the batch's `total_input_coords=51595` is four orders of magnitude below the falsified 10 M-coord threshold, so the old warning never fires at all. This is precisely the planet failure mode at country scale: a modest-coord feature covering a vast tile area, invisible to the coord predictor, caught by the area predictor. On the planet, `predict_pieces` on the real monster (656343) will exceed 1.14 M and trip this warning on the line before the render — the death signature the last planet run lacked.

**A bytes-vs-pieces caveat, stated so the record is honest.** great-britain averages 151 bytes/piece, but the byte-max flush above is 1,930 bytes/piece — so the byte-max and piece-max flushes are different batches, and this summary tracks the byte-max one. It therefore does **not** give feature 784440712's own emitted-piece count, so no per-feature over/under-prediction can be read from it directly. What it does bound: since every predicted term over-counts, 784440712's total over-prediction is at most its own 143,100-piece prediction, i.e. ≤ 0.3% of the run — no single feature meaningfully dents the 1.034× aggregate.

## Cost

No pipeline slowdown: the added work is one bounding-rect reduction plus O(zooms) arithmetic per feature, dwarfed by the render it precedes. great-britain wall / peak-RSS, `main` vs branch, was 175 s / 5.29 GB vs 167 s / 5.14 GB — the branch nominally faster, i.e. within noise on a box under load ~10 from concurrent sessions. A clean europe A/B is deliberately not run here: a log-only change cannot regress the common path, and the box was too loaded to time honestly. europe A/B belongs to the fix PR, where the *bounding* mechanisms can actually move the number.

## Known gaps (stated plainly)

- The over-prediction invariant is unit-tested on representative geometries and holds in aggregate on GB (1.034×), but no property/fuzz test yet proves `predicted >= emitted` per feature over arbitrary real geometry, and a sub-ULP under-count is possible exactly on a tile boundary (cosmetic in a diagnostic log; `predict_pieces` never feeds output, so byte-identity is untouched). Owed to the fix PR, where the predictor becomes load-bearing for a memory budget: round the rect outward and add the property test.
- Per-feature predicted-vs-actual is not captured — the summary rides the byte-max flush. Capturing the top-K features' individual fidelity needs the `flush_render_batch` restructure the fix PR does anyway; fold it in there.
- `predict_pieces` ignores `options.bounds`, so it over-predicts for features extending beyond a bounded source's bbox. Intentional (the safe direction) for this measurement; a bounds-clip intersection is the candidate tightening for mechanism 3. It is moot for the planet (unbounded), whose real monsters are solid ocean/ice polygons that fill their bbox — where the predictor is already tight.

## Mechanism 2: predicted-pieces chunking (byte-identical, local-verified)

The first bounding mechanism, built on the predictor above. `flush_render_batch` now computes each feature's predicted pieces once, cuts the batch into contiguous sub-chunks whose summed prediction stays under a budget (`RENDER_CHUNK_PIECES = 32,768`, ~256 MB at the tag-heavy monster rate), and renders + feeds each sub-chunk in turn (`render_chunk`), freeing its `rendered` Vec before the next. The H-A wall was a cluster of monster multipolygons in **one** batch rendering concurrently into one multi-GB Vec (≥ 35-40 GB in flight at the kill); chunking renders them sequentially, one bounded sub-chunk at a time, so peak render memory drops from a whole batch of monsters to one sub-chunk — or, until slab-mode lands, one single over-budget feature.

**Byte-identical by construction.** Sub-chunks are contiguous, in batch order, and each `par_iter().flat_map_iter().collect()` preserves order, so the concatenated `sort.add` sequence is unchanged from a single whole-batch collect — and `render_feature` is a pure function of one feature, so a feature renders the same in a sub-slice as in the whole batch. The partitioner `chunk_ranges` is unit-tested for exactly the property this rests on: the ranges tile `0..n` contiguously with no gaps or overlaps, and a single over-budget feature forms a singleton.

| config | mech 2 vs increment-1 (== `main`) |
|---|---|
| monaco z12-14 | identical (common path, single chunk) |
| rhode-island z12-14 (default + spill) | identical (common path) |
| great-britain z12 sparse (planet flags) | **identical — and the chunking path actually engaged** |

On great-britain the monster batch split as intended: flushes **10,584 → 10,611**, and the peak flush dropped from **40,154 pieces / 77 MB to 12,893 pieces / 42 MB** — the working set nearly halved with byte-identical output. Wall time was 158 s (mech 2) vs 160 s (baseline), i.e. no regression from the chunking. On the planet, where the batch holds several multi-GB monsters at once rather than GB's lone country-scale feature, the reduction is from the concurrent multi-monster aggregate down to the largest single sub-chunk — the mechanism that should let the run complete.

### europe A/B (continent scale, `main` 30d8b01 vs branch 06ea300, 2026-07-21)

Interleaved M·B·M·B, best-of-2, planet-representative flags (`--storage sparse -z12 --min-size 0 --simplify 0 --buffer 0 --sort-memory-mb 1024`), isolated TMPDIR per run, `/usr/bin/time -l`. The only heavy job on the box was the run itself (verified via `ps` — no competing pipeline; the high load numbers were the run's own pass-2/render phase).

| run (order) | wall | peak RSS | stage1 render | stage2 write |
|---|---|---|---|---|
| main r1 (1st) | 2400.0 s | 51.90 GB | 1745.5 s | 654.0 s |
| branch r1 (2nd) | 2568.1 s | 51.48 GB | 1862.2 s | 705.6 s |
| main r2 (3rd) | 2485.3 s | 51.59 GB | 1774.7 s | 710.1 s |
| branch r2 (4th) | 2530.0 s | 51.58 GB | 1843.6 s | 685.9 s |

**Byte-identical, confirmed at content level.** All four runs produced identical entries (624,733,498) and tiles (739,799); the two output archives share the same sqlite SHA3 content hash on both `tiles_data` (`b97486f6…`) and `tiles_shallow` (`9bc4ae38…`), with identical `metadata` — a true logical-content match on europe, on top of the full per-tile ladder (monaco / RI / GB) above. **Peak RSS is variant-neutral** (51.5–51.9 GB, node-store-dominated; branch never higher than main).

**Correction to the pre-run expectation — the record should show it.** The plan expected europe to stay single-chunk ("no relation of that class"), and this session's status updates initially repeated that. It is wrong: the predictor named **13 monster features** and chunking **split 568 batches** (branch 149,539 flushes vs main 148,971). europe has real monsters, just sub-planet-scale — and bounding them helped: the **peak render flush dropped 603 MB → 406 MB** byte-identically (main max_flush 159,164 pc / 632,952,289 B; branch 72,906 pc / 425,688,702 B). europe's monsters clear the 32,768-predicted chunk threshold but sit far below the planet's 8.8 GB, so the flush reduction doesn't move the node-store-dominated total RSS — but it demonstrates the mechanism engaging correctly on real continent data, not just in the GB smoke.

**Cost, measured against a control rather than asserted.** The interleave puts branch always in even positions (2nd/4th), so variant is confounded with run-order / thermal / disk-warmth drift. The **write stage (stage2) is the control** — mechanism 2 cannot touch it — and it moves: branch's write mean is +2.0% over main's (695.8 vs 682.0 s), and the same-binary pair main-r1 vs main-r2 (no code change, later position) swings **+3.5% wall / +8.6% write** on its own. That fixes the run-to-run noise band at ~8%. Branch's render stage (stage1) is +5.3% over main's (1852.9 vs 1760.1 s mean); netting out the +2.0% the untouched write stage attributes to position leaves **~3% of the render stage (~2% of wall)** as mechanism 2's real cost. It is the `predict_pieces` per-feature precompute (one bounding-rect pass), not the chunking (568 split batches out of 149 k is 0.4%). An acceptable price for the working-set bound that makes the planet completable; a later fusion of the predict bbox into render's existing coord pass could reclaim most of it.

**Residual → mechanism 3.** europe's post-mech-2 peak flush (72,906 pc / 406 MB) is a *single* feature (predicted 430,606) rendering whole as a singleton chunk — exactly slab-mode's target. So europe joins great-britain as a mechanism-3 validation datapoint.

## Mechanism 3: slab-mode (byte-identical, local-verified)

The intra-feature bound. Mechanism 2 caps a *batch* to one sub-chunk, but a single feature predicted over the chunk budget is isolated by `chunk_ranges` as a singleton and still renders its whole piece set into one `Vec` (measured 8.8 GB for planet feature 656343). Slab-mode renders such a feature in bounded **slabs** and feeds the sort per slab, so its resident output never exceeds one slab regardless of size.

**Shape.** `render_at_zoom`'s per-zoom setup and per-tile clip are factored into a shared `PreparedZoom` (`prepare_zoom` + `piece`), so the common path and slab-mode derive an identical rect and clip from one source of truth — the common path is otherwise unchanged. A new `render_feature_slabbed` walks each zoom's covering tiles (the same tx-outer/ty-inner order `render_feature` emits) in contiguous `RENDER_SLAB_TILES`-sized slabs, par-clipping each and handing it to a caller sink; parallel clipping is kept *within* the slab, so the bound costs resident memory, not clip throughput. `render_chunk` routes a singleton feature predicted over the chunk budget through it (`chunk.len() == 1 && predicted[0] > RENDER_CHUNK_PIECES`), encoding + feeding each slab in order. `RENDER_SLAB_TILES = RENDER_CHUNK_PIECES` (32,768), so exactly the features chunk_ranges isolates are the ones that slab.

**Byte-identical — full ladder re-run vs `main` (30d8b01), all four configs.** Real sqlite SHA3 on `tiles_data` + `tiles_shallow`:

| config | mech 3 vs `main` (30d8b01) |
|---|---|
| monaco z12-14 | identical (`tiles_data 108ec160…`) |
| rhode-island z12-14 (default) | identical (`tiles_data ed10bdc9…`) |
| rhode-island z12-14 (`--sort-memory-mb 8`, spill) | identical (same hash) |
| great-britain z12 sparse (planet flags) | identical (`tiles_data 69e44a59…`, 21324 / 18544) |

A `render::tests` unit test (`slabbed_render_matches_whole_and_bounds_slabs`) proves the multi-slab path directly: a filled polygon covering ~100 single-zoom tiles, slabbed at budget 8, emits >1 slab, each ≤ budget, and their concatenation equals `render_feature(..)` piece-for-piece in order — the boundedness + byte-identity the fix rests on.

**What great-britain did and did not show — stated honestly.** GB is byte-identical and its one over-budget feature, 784440712 (predicted 143,100), *does* route through the slab branch — but the flush count is **unchanged** (10,611) and the peak flush is **unchanged** (12,893 pc / 42 MB). The reason is real: `predict_pieces` over-counts from the raw lng/lat bbox and *ignores the source-bbox clip* (a known gap), while the actual covering rect `prepare_zoom` computes is clipped to GB's header bbox and thinned by the maxzoom min-size — so 784440712's *actual* rect fits inside a single 32,768-tile slab. GB has no feature whose clipped rect exceeds one slab, so it exercises the slab *branch* but not a multi-slab *split*. GB's memory peak (42 MB) is a different, tag-heavy feature (60380683, predicted 15,732) that is under budget and already bounded by mechanism 2. So on GB mechanism 3 is correctly inert on memory; the multi-slab split and its reduction show where a feature's *clipped* rect exceeds a slab — europe (72,906-piece feature, running) and the planet's 656343 (Phase 3).

**Probe attribution fixed in the same increment.** The slab branch records **each slab as its own flush** (not the accumulated whole-feature total), so the RenderProbe's `max_flush_bytes` reflects the real resident bound — one slab — rather than reporting 656343 as an 8.8 GB flush and hiding the mechanism in the very log meant to show it. Byte-identity is untouched (the probe never feeds the sort). The `profile` a slab flush carries is the whole feature's, so a slab flush's *predicted* pieces name the monster while its *realized* pieces show the slab bound — a deliberate, documented attribution.

**europe multi-slab demonstration (continent scale — the proof GB could not give).** Where GB's one over-budget feature fits a single clipped slab, europe has **13** features whose clipped rect genuinely exceeds a slab (predicted up to 430,606 — feature 29786503, europe's mech-2 406 MB peak). A full mech-3 europe run (planet flags, `--sort-memory-mb 1024`) slabbed all thirteen: **peak render flush dropped 406 MB → 148 MB** (72,906 → 19,468 pieces, **−63%**), the lone-monster ceiling replaced by an ordinary near-budget chunk, with **+156 slab flushes** (149,539 → 149,695) as the positive signature of the splits. **Byte-identical**: `tiles_data` SHA3 `b97486f6…`, identical to the recorded mech-2 europe hash, with 739,799 tiles and 624,733,498 entries unchanged. Slab-mode splits a real monster and bounds its resident output while changing nothing on the wire — and on the planet, where 656343 is a solid ocean polygon of ~1.14 M *actual* pieces (not GB's thin coastline), the same mechanism turns its 8.8 GB flush into ~one slab.

## Next

Mechanisms 2 + 3 are the local-verifiable fix and are done: byte-identical on the full ladder, cost measured (~2–3% render, europe), the intra-feature bound proven by unit test. **Owed to this PR before merge:** a property/fuzz test for the `predicted >= emitted` invariant; optional per-feature fidelity capture and a `predict_pieces` bounds-clip tightening (the GB finding above shows the clip gap concretely — intersecting the predicted rect with `options.bounds` would tighten it). **Mechanism 1** (input-side early flush by summed input size) is defense-in-depth on the input `batch` Vec and may be skipped if 2 + 3 suffice — the planet run decides. **Then Phase 3** (cloud, John's go, ≤ $25): the uncapped planet z12 run + Java v0.10.2 head-to-head. The pre-render WARN ships the death signature the last planet run lacked, and the per-slab probe now shows the bound in action, so 656343 (predicted > 1.14 M) is named before it renders and bounded to ~one slab as it does.
