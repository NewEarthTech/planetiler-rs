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

**Owed before cloud:** the europe A/B (wall + peak RSS, `main` vs branch) to confirm the chunking adds no common-path overhead at continent scale. Deferred from this session because the box was under load ~10 for most of it; the local ladder above plus the GB timing are the interim evidence, and europe is the next session's first rung now that the box is quiet.

## Next

Mechanism 2 caps the batch term; **mechanism 3 (slab-mode)** removes the remaining single-over-budget-feature ceiling by iterating one monster's tile rect in bounded slabs, feeding the sort per slab (preserving the z-ascending tx-outer/ty-inner order, so still byte-identical). Mechanism 1 (input-side early flush) is defense-in-depth on the input batch and may fold in or be skipped if 2+3 suffice. Then Phase 3: the validated planet run + Java head-to-head. The predicted-pieces pre-render warning already ships the death signature the last planet run lacked, so any residual wall names its monster (e.g. 656343, predicted > 1.14 M) on the line before it dies.
