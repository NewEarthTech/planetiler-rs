# Planet head-to-head: the first complete run

The H-A fix (#35) survives the planet. First complete planetiler-rs planet run -- z12, the full 93.9 GB `planet-260713.osm.pbf`, clean exit -- followed by the pinned Planetiler v0.10.2 on the same box and input. The run doubles as the instrumented probe the probe-first rule requires: it names the critical path per phase, with per-phase CPU utilization from the memory sampler, and it licenses the next two parallelism campaigns. Raw receipts: `planet-run-20260722.stderr`, `planet-run-20260722-mem.log`, `planet-run-20260722-report.txt`.

## Configuration

- Box: r7gd.8xlarge (32-core Graviton3, 247 GB, 1.7 TB instance-store NVMe scratch), us-west-2, kernel 6.17.0-1019-aws; rustc 1.97.1; OpenJDK 21.0.11.
- Input: `planet-260713.osm.pbf`, 93,874,282,582 bytes, pinned from `osm-planet-us-west-2` (in-region S3), length-verified before either run.
- planetiler-rs at `main` `58b9a33` (#35): `generate --layer osm --minzoom 12 --maxzoom 12 --storage sparse --sort-memory-mb 1024 --min-size 0 --simplify 0 --buffer 0` (the standard parity flags).
- Planetiler v0.10.2 pinned jar, `osm-qa` profile, stock defaults (`nodemap_type=sparsearray`, `nodemap_storage=mmap`), same tmpdir volume.
- rs ran first, Java second (`parity/cloud/remote-planet.sh`); no other load on the box.

## Scoreboard

| tool | wall | peak memory | tiles | bytes |
|---|---|---|---|---|
| planetiler-rs (sparse) | 2h 55m (10,527 s) | 117 GB VmRSS / **24 GB anon** | 13,297,463 | 77,856,399,360 |
| Planetiler v0.10.2 | **38m 37s** | 121.7 GB MaxRSS | 13,120,762 | 82,082,721,792 |

**Java is 4.5x faster at planet scale, and peak RSS is a wash.** Both numbers deserve their context. The RSS tie is an mmap artifact: both sides page ~80 GB of node values through the page cache, and resident-set accounting charges it to whoever touched it. The honest planetiler-rs heap figure is the **24 GB anonymous peak** -- the quantity that was a 2.8 GB/s runaway death on 2026-07-20 and is now flat. Java's MaxRSS from `time -v` cannot be decomposed the same way, so no memory-advantage claim is made at planet scale from this run; that claim needs a constrained-memory A/B (cgroup limit or a smaller box), which is cheap and still open.

The 4.5x is real, and the rest of this file is about where it lives.

## Phase budget and utilization

The memory sampler (1,011 samples, ~10 s cadence, `planet-run-20260722-mem.log`) covers the whole rs run; joining it against the phase boundaries in the stderr gives per-phase CPU utilization on the 32-core box:

| phase | wall | avg load | peak load | utilization |
|---|---|---|---|---|
| pass 1 (nodes, 10.74 B) | 20.3 m (1,220 s) | 3.8 | 15.6 | ~12% |
| pass 2 (ways) | 74.2 m (4,450 s) | 3.7 | 19.0 | ~12% |
| relation + render tail | 40.8 m (2,446 s) | 23.1 | 30.8 | ~72% |
| merge + encode + write | 40.2 m (2,410 s) | 6.4 | 22.4 | ~20% |

Integrating load over the run: planetiler-rs spent **~25.8 core-hours** of CPU. Java's ceiling on the same box is 20.6 core-hours (38.6 m x 32, its load was not sampled), so **the total work ratio is at most ~1.4x -- the 4.5x wall-clock gap is scheduling, not computation**. At the current per-core efficiency, perfect scaling alone would land the planet at **~48 minutes**. Java's 38m37s on identical hardware is the existence proof that the box can be kept fed.

The shape of the ways/render coupling matters for what to fix first: render runs starved behind pass 2 (combined load 3.7 for 74 minutes), then drains a 41-minute backlog at load 23. The two phases are one problem -- feed render faster and the tail largely dissolves into overlap.

## H-A closure: the predictor at planet scale

The mechanism that killed the 2026-07-20 attempt is dead:

- Largest flush of the entire run: **28,116 pieces / 521.5 MB**, vs the fatal single-feature **1,145,294-piece / 8.80 GB** flush recorded in `planet-probe-20260720.stderr`.
- **251 pre-render monster warnings** fired, each naming its feature before rendering it. Largest predicted expansion: **854,932 pieces** (input id 170017643), chunked without incident. Zero large-working-set warnings anywhere in the run.
- The over-count invariant held globally (`total_predicted_pieces >= total_pieces` at every summary), but cumulative over-prediction grew from ~1.03x early to **~4.7x** by the end: the bbox-rect bound over-counts badly on huge sparse geometries (coastline-class features cover few of the tiles their bounding box spans). Correctness-safe, but it over-chunks the monsters -- a named tightening candidate, with the offending ids in the log.
- Sparse node store at planet scale confirmed the design prediction from the scoreboard: 10,742,674,832 nodes in 496,166,544 runs, **avg run length 21.65** (rhode-island was 4.9), values at **7.6 B/node** (81,960 MB) plus a 7.4 GB run index.

## Merge-stage decomposition (from the run log, no new probe needed)

Within the 40-minute merge + encode + write stage, the queue-depth proxies settle it: `batches_sent - batches_done` hovers at ~zero across all 53,852 batches, and `tiles_completed - tiles_written` likewise. Encode and write are keeping up; they are starved. **The single k-way merge cursor over 369 spilled chunks is the governor, feeding ~0.64 M entries/s** (1.554 B entries / 2,410 s) while the stage averages load 6.4.

This is planet-scale evidence, distinct from the 2026-07-03 partitioned-merge experiment that was falsified at great-britain/europe scale (see `osm-qa-headtohead.md`); that branch stays parked as its own receipt. Any new merge mechanism starts from this measurement and must re-prove itself at the scale that produced it.

Worst tile of the planet, for future adversarial testing: **1,021,887 entries / 352.8 MB** pre-encode at z12 x=3430 y=1753 (TMS).

## Tile-set fringe vs Java

The tile-count disagreement is the known fringe, now measured at four scales: rhode-island **+14** (2,217 vs 2,203), great-britain **+132** (21,472 vs 21,340), europe **-99** (739,799 vs 739,898), planet **+176,701** (+1.33%, 13,297,463 vs 13,120,762), with rs output 5.1% smaller in bytes. The planet archives died with the box (only logs ship home), so planet-scale characterization goes through the country-scale reproduction:

Reproduced 2026-07-23 on current `main` (`58b9a33`), 12-core macOS, both tools on `great-britain.osm.pbf` with the parity flags: **rs 21,324 tiles vs Java 21,340 -- rs-only 0, java-only 16 -- the rs tile set is a strict subset of Java's at this scale** (note the table row in `osm-qa-headtohead.md` showing rs +132 predates the relation/bounds fixes; the fringe flipped sign since). The 16 missing tiles decode to exactly two features: the ten identical 421-byte tiles are the interior fill of the **Berwick Bank Offshore Wind Farm** polygon (closed way, `seamark:production_area`, off the Scottish east coast), and the five 189-190-byte tiles are Channel traffic-separation-scheme seamark zones (`Dover Straits TSS` sampled), plus one 20-byte allocated-but-empty tile. Both named features are large offshore areas whose interiors span whole z12 tiles: Java's osm-qa promotes these closed ways to polygons and emits full-tile interior fills, while rs renders them as rings, so tiles containing nothing but interior never materialize. **Working hypothesis: closed-way area-promotion semantics (which tags make a closed way a polygon) diverge between the implementations.**

Shared-tile parity is unchanged in character: of 21,324 shared tiles, 3,158 byte-identical, 4,142 semantically equal, and the 14,024 remainder is the known sub-pixel class (first diff: one quantization unit on a submarine-cable vertex, `(0, 2284)` vs `(0, 2285)`).

The planet-scale **+176,701 has the opposite sign (rs superset), so it is not this mechanism alone**: at planet, the largest water/coastline polygons dominate and rs's fill coverage evidently exceeds Java's somewhere -- plausibly the same area-semantics divergence pointing the other way on a different feature class, or Java's large-tile handling. Characterizing it needs tile-set receipts from a future planet run (ship `tiles_shallow` coordinates home, not just counts). Open follow-up; it does not block the perf campaigns, but it does block any output-quality claim vs Java.

## Named next levers, in order

1. **Pass 2 (ways) parallelism** -- 74 minutes at 12% utilization; this run is the licensing probe. The hot loop is ~11 B node lookups against the read-only sparse store (binary search over 496 M runs, cache-hostile, effectively serial). Ways are independent given the store; parallel assembly with ordered handoff is the house pattern. Super-linear payoff: the 41-minute render tail collapses into overlap as the feed speeds up (~115 minutes -> ~30 plausible).
2. **Merge feed parallelism** -- 40 minutes governed by one cursor at 0.64 M entries/s with downstream starved. Licensed at planet scale by this run; parked europe-scale falsification stands as the cautionary receipt.
3. **Pass 1 (nodes)** -- 20 minutes at 12% utilization, same disease as pass 2.
4. **Predictor tightening** for the sparse-monster class (4.7x cumulative over-prediction -> unnecessary chunking overhead on exactly the features that flush largest).

The bound worth remembering: measured work says ~48 minutes at perfect scaling with today's per-core efficiency, before any inner-loop optimization. The gap to close is idle silicon first, arithmetic second.
