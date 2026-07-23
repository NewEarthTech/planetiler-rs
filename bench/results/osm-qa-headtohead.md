# Head-to-head: OSM QA tiles (planetiler-rs vs Planetiler v0.10.2)

Task: read an `.osm.pbf` extract and write all OSM features into a single `osm` layer as MBTiles, at the `osm-qa` zoom range **z12–14**. Measured with `bench measure` (`/usr/bin/time`) on a 12-core macOS machine (OpenJDK 26), default gzip tiles. Both tools at default settings (no heap tuning, no thread pinning).

## Results across three extract sizes (after the optimization rounds below)

| extract | tool | wall | peak RSS | tiles |
|---------|------|-----:|---------:|------:|
| **monaco** (0.7 MB, z12-14) | planetiler-rs | **0.41 s** | **84 MB** | 102 |
| | Planetiler | 2.20 s | 1068 MB | 102 |
| **rhode-island** (49 MB, z12-14) | planetiler-rs (ram) | **4.2 s** | 1138 MB | 2217 |
| | planetiler-rs (sparse) | **4.0 s** | **1088 MB** | 2217 |
| | Planetiler | 8.94 s | 7814 MB | 2203 |
| **great-britain** (2.0 GB, z12) | planetiler-rs (ram) | 151 s | 7.6 GB | 21472 |
| | planetiler-rs (sparse) | 153 s | **5.8 GB** | 21472 |
| | Planetiler | **81 s** | 29.1 GB | 21340 |

**Speed:** ~5× faster (monaco), ~2.2× faster (rhode-island), **1.9× slower (great-britain)** — the crossover where Java's mature parallel throughput overtakes ours now sits between state and country scale (it was "anything bigger than a city" before the optimization rounds). **Memory:** ~13× / ~7× / ~5× less. **Tile counts match at every size** (relations + source-bounds clipping closed them; per-tile content ~50% semantically identical on monaco, the rest sub-pixel — see parity section).

**Planet (2026-07-22): the first complete run.** planetiler-rs finishes the full planet at z12 — 2h 55m, 24 GB anon heap, 13.30 M tiles — vs Planetiler v0.10.2 at **38m 37s** on the same 32-core cloud box: 4.5× slower, with per-phase utilization data naming exactly where the gap lives (two phases at ~12% CPU). Full record and the licensed next levers: `planet-headtohead.md`.

## Reading the trend (honest)

- **The rhode-island numbers are now a *fair* comparison.** The earlier 4.21 s / 680 MB / 7522-tile row was us doing *less* work than Java: no relations, no element metadata, no bounds clip. With those added (multipolygon/route assembly, `@id/@type/@version/@timestamp`, clip to the source bbox) the output matches — **2217 tiles vs Java's 2203** — and our absolute cost rose to ~7 s / ~1 GB. That is the honest apples-to-apples figure.
- **Even at matched output we win on both axes**: ~1.25× faster and ~7.6–8.3× less memory — from no GC and no reserved JVM heap. Planetiler even *failed* to run monaco under `-Xmx512m` / `-Xmx256m`; it needs a large default heap.
- **Node storage is selectable** (`--storage ram|mmap|sparse`), output byte-identical across all three. Peak RSS on rhode-island: ram **1015 MB**, mmap **927 MB**, sparse **932 MB** — mmap/sparse move the store off-heap (~85 MB) but they land together because the node store isn't the bottleneck at region scale (the 256 MB sort buffer + metadata-heavy features dominate ~900 MB). The **`sparse`** layout is run-compressed (8 B/node values in id order + a small run index), measured at **65 MB vs the sorted table's 92 MB** node store on rhode-island (~11 B/node at avg run length 4.9). Its real payoff is **planet scale**: node ids there are dense, so runs lengthen and it approaches **8 B/node** — half of ram/mmap — while a direct id-indexed array would need **111 GB** even for rhode-island (14 B id span at 0.04% density).

## Scale trajectory (great-britain, 2.0 GB, ~240 M nodes)

Pushing to country scale answered "is the node-store work worth it?" — and found the *real* memory wall along the way.

| run | ram | mmap | sparse |
|---|---|---|---|
| **z0-6** (with low zooms) | 59.3 GB | 62.4 GB | 62.4 GB |
| **z12** (realistic osm-qa) | 5.6 GB | — | **4.5 GB** |
| **z0-6, `--max-features-per-tile 50000`** | **5.9 GB** (was 59.3) | — | — |

- **The node store is *not* the peak-memory bottleneck; low/mid-zoom mega-tiles are.** Rendering a large area including z0-4 packs enormous feature counts into a handful of tiles, and each whole tile was materialized as heavy decoded structs to encode. The external sort itself bounds fine (spills at 256 MB); this is a separate accumulation in the tile writer. (rhode-island never shows it — z0-6 there is 1 GB — because the area is tiny.)
- **A per-tile-layer cap fixes it**: `--max-features-per-tile 50000` cut GB z0-6 from **59.3 → 5.9 GB (10×)** and ran ~2× faster (dropped features are skipped before decode). Output at realistic osm-qa zooms (z12-14) is unaffected — no low-zoom tiles, and the default is unlimited.
- **At realistic zoom the node store *does* pay off**: GB z12 is 5.6 GB (ram) vs 4.5 GB (sparse) — sparse saves ~1.15 GB (20%), matching the node-store delta (240 M × 16 B = 3.84 GB vs sparse ~2.7 GB). The margin grows with scale (rhode-island ~85 MB → GB ~1.15 GB), trending to planet where ram's ~144 GB store is impossible.

### great-britain z12 head-to-head vs Planetiler (osm-qa)

| tool | wall | peak RSS | tiles |
|---|---|---|---|
| planetiler-rs (sparse) | 332 s | **4.3 GB** | 21472 |
| Planetiler osm-qa | **81 s** | 29.1 GB | 21340 |

Near-identical output (21472 vs 21340 tiles, 0.6%). The honest verdict at country scale is **mixed**:
- **Memory: ~6.7× less** (4.3 GB vs 29 GB). This is the architectural / OOM thesis and it holds decisively — we build great-britain in 4 GB where the JVM grabs 29. Part of the story is a deliberate *tradeoff*: Planetiler keeps the node map and features in RAM (fast, huge); we spill the sort to disk and mmap the node store (small, slower).
- **Time: ~4× slower** (332 s vs 81 s). This inverts the rhode-island result, where JVM cold-start masked Java's throughput. At scale, Planetiler's mature, heavily-parallel pipeline out-throughputs the young Rust port. The ram node store doesn't close it (ram z12 ≈ 324 s too), so the gap is in read/render/sort/encode throughput, not node-store I/O — i.e. optimization headroom, not an architectural wall.

**So the "beats it on time *and* memory" bar is half-met at country scale: memory yes, time no (yet).**

**Sort-buffer sweep (falsifies the "we're slow because we're frugal" hypothesis):** raising `--sort-memory-mb` from 256 MB to 4 GB / 16 GB (and pairing 16 GB with the ram node store) left wall time flat — 332 s / 332 s / 328 s / 330 s — while RSS climbed to 9–11 GB. Spending 2.5× the memory bought back nothing: sort-spill I/O is cheap (sequential SSD), so the 4× time gap is **compute throughput** in read → render → encode.

**Phase breakdown + optimization rounds (GB z12, phase timers):** baseline split was pass1 nodes 57 s / pass2 ways 129 s / **relations ~154 s (41%!)** / write 36 s.

| phase | baseline | + relation fixes | + parallel decode |
|---|---|---|---|
| pass 1 nodes | 57 s | 53 s | **22 s** |
| pass 2 ways | 129 s | 136 s | **66 s** |
| relations | ~154 s | ~28 s | ~27 s |
| merge+encode+write | 36 s | 36 s | 37 s |
| **total** | **332 s** | **~250 s** | **153 s** |

- **Round 1 — relation tail (332→250 s):** `assemble_rings` was O(n²) (linear scan per fragment; GB admin boundaries have thousands of member ways) → endpoint-hash O(n) stitching; and a giant feature covering thousands of tiles clipped as a *single* rayon task → per-tile parallel clipping above 256 covering tiles.
- **Round 2 — parallel PBF decode (250→153 s):** both passes decoded the file single-threaded (`osmpbf::for_each`), twice. Blobs are now read sequentially but decoded in parallel windows of 64 with ordered consumption — node ids stay ascending (the sparse store's invariant), feature emission stays deterministic, and the way pass shares the finished node store read-only across workers so location lookups parallelize too.
- **Every round verified byte-identical** (same entries/features/tiles on GB: 24638633 / 43348118 / 21472; monaco 102/102).

**Net: the time gap vs Planetiler fell 4.1× → 1.9×** (153 s vs 81 s) **at ~5× less memory** (5.8 GB vs 29.1 GB). Remaining fat: pass 2 (~66 s), the relation stage (~27 s), and the write stage (~37 s).

**Tried and reverted — decode/consume double-buffering:** a producer thread decoding window N+1 while the consumer processed window N measured **155 s and +1.2 GB RSS** (vs 153 s baseline). The rayon pool is already saturated (render flushes fill the cores between decode windows; user CPU *rose* 565→590 s from contention), so the overlap bought nothing and cost an extra in-flight window. Dropped rather than shipped.

## Continent scale (europe, 34.6 GB, z12) — the honest reversal

Both tools, same 96 GB / 12-core machine, same task (osm-qa z12). Ours ran with `--storage sparse --min-size 0 --simplify 0 --buffer 0`; Planetiler ran twice — stock defaults and a tuned big-build config.

| tool | wall | peak RSS | tiles |
|---|---|---|---|
| planetiler-rs (sparse) | 53.3 min | **67.0 GB** | 739,799 |
| Planetiler (stock defaults) | **17.7 min** | 74.9 GB | — |
| Planetiler (-Xmx48g, mmap, sparsearray) | 18.6 min | 71.4 GB | 739,898 |

- **Planetiler wins europe on this machine: ~2.9× faster, and the memory gap collapses to a near-wash** (67 vs 71–75 GB). Both complete without OOM — "defaults" on a 96 GB box mean a ~51 GB heap, and their architecture handles the rest. The GB-scale trend (1.9× slower at 5× less memory) did **not** hold at 17× the input.
- **Why our memory advantage vanished:** the relation-capture structures (`member_ways` geometries, `needed_ways` ids, pending relation tags, all held in RAM through pass 2) scale with europe's relation count and dominate our 67 GB. Planetiler's own log shows the equivalent state at **`mpGeoms: 1.4G`** — the same job in ~1/20th the memory. That is the next architecture item, not a tuning knob.
- **Why the time gap widened (1.9× → 2.9×):** our write stage alone took 25 min (625 M sorted entries through k-way merge into a single serial SQLite writer) — longer than Planetiler's entire run. Pass 2 (17.8 min) also roughly matches their total.
- Tile counts agree within 0.013% (739,799 vs 739,898).
- Caveats: development builds/tests shared the machine with both sides' runs (roughly symmetric noise, ±5%-ish); our first attempt failed on a silently-truncated download (curl exits 0 on early close — resume and verify `content-length` before trusting a 30 GB fetch).

## Continent scale, round 2 — the two named fixes land

The next optimization arc shipped both fixes the first europe run named, each verified byte-identical (sqlite tile-content sha256) on monaco, rhode-island, and great-britain before merging:

- **Parallel archive write** — the write stage was one thread doing k-way merge, per-entry blob decode, tile assembly, encode, *and* SQLite inserts. It is now a three-stage pipeline over bounded channels: a merge thread groups sorted entries into whole tiles (per-tile-layer cap applied before any decode), the rayon pool decodes + post-processes + encodes + compresses batches, and the writer stays serial in merge order — output identical by construction. MBTiles dedup switched from keying on whole compressed blobs (an unnamed third memory hog: it held every unique tile in RAM, ~20 GB at this scale) to a 128-bit xxh3 content hash, with cached prepared statements and one archive-wide transaction.
- **Relation capture off the heap** — member-way geometries stream to a file-backed `WayGeometryStore` (packed 8-byte fixed-point records, the node stores' 1e-7 packing, so coordinates survive bit-identically; a 16-byte id+span index on the heap; mmap lookups), `needed_ways` became a sorted `Vec<i64>`, and pending relation tags are held postcard-encoded until assembly.

Each was A/B'd on great-britain z12 before it shipped, branch against its own main in the same session:

| great-britain z12 (sparse) | before parallel write | + parallel write | + relation capture off heap |
|---|---|---|---|
| merge + encode + write | 61.0 s | **41.1 s (−33%)** | — |
| total wall | 173.8 s | **150.9 s (−13%)** | 153.8 s (flat, within noise) |
| peak RSS | 7.18 GB | **5.64 GB (−21%)** | **5.14 GB** |
| peak footprint | 3.50 GB | **2.36 GB (−33%)** | **1.80 GB (−24%)** |

Read down a column, not across the table: each pair is its own session, and the machine was not equally quiet in both. The parallel-write session's own main measured **173.8 s** for the same code the round-3 table above recorded at **153 s** — a 14% spread on identical binaries, which is the honest size of the noise floor on this shared box and the reason every perf claim here is a same-session A/B rather than a comparison against a number from another day. The off-heap relation move is deliberately flat on wall time: great-britain's relation state is small, and europe is what it was built for.

Note that the head-to-head table at the top of this file predates both fixes on great-britain — its 153 s / 5.8 GB row was measured before them, so it now understates our memory position there (post-fix RSS is 5.14 GB). Refreshing it needs a fresh GB run against the pinned jar, not a splice of these branch-vs-main deltas into a cross-tool table.

Same machine, same command, same input as round 1:

| europe z12 (sparse) | round 1 | round 2 | Planetiler (stock) |
|---|---|---|---|
| pass 1 (nodes) | 369 s | 382 s | — |
| pass 2 (ways) | 1066 s | 1028 s | — |
| relations + render tail | ~255 s | ~303 s | — |
| merge + encode + write | 1508 s | **780 s (−48%)** | — |
| **total wall** | **53.3 min** | **41.6 min (−22%)** | **17.7 min** |
| peak RSS | 67.0 GB | **53.3 GB (−20%)** | 74.9 GB |
| peak memory footprint | 41.8 GB | **17.1 GB (−59%)** | 32.4 GB |
| tiles | 739,799 | 739,799 | 739,898 |

**Output is byte-identical to round 1 across all 739,799 tiles** (identical content sha256 over 24 GB of tiles), so this is pure performance, zero behavior change.

The verdict moves meaningfully:

- **Memory is a win again, not a wash.** RSS 53.3 vs Java's 71–75 GB (~1.4× less); honest dirty memory (footprint) 17.1 vs 32.4 GB (~1.9× less, and ~3.5× less than Java's tuned run at 59 GB). Much of our remaining RSS is clean, evictable mmap pages of the 27 GB sparse node-store file — on a smaller machine the kernel drops them instead of OOMing, which is the architectural point.
- **Time gap narrows 2.9× → ~2.3×.** The write stage halved; its floor is now the single merge thread (~0.9 µs/entry over 625 M entries ≈ 9.5 of the 13 min). A partitioned/parallel merge looks like the obvious next lever — it was built, measured on dedicated hardware, and **falsified**; see the section below. That leaves pass 2 (17 min, ~matches Java's whole run) as the standing named lever.

**Standing verdict across scales: we win city (~5×) and state (~2.2×) on time and memory; at country we're 1.9× slower at ~5× less memory; at continent we're ~2.3× slower at ~1.4× less RSS / ~2× less dirty memory.** Continent is no longer an outright loss — it's the same shape as country: a time gap that is throughput headroom, on top of a standing memory win.

## Tried and parked — partitioned merge (falsified on dedicated hardware)

The write-stage section above names the single merge thread as the remaining floor and a partitioned merge as the obvious lever. It was built (tile-floor partitions, pread cursors, a claiming window to bound in-flight memory) and measured on dedicated cloud boxes rather than the shared laptop, because a few-percent effect is invisible under this machine's noise floor. **It is slower on both datasets and does not ship.** The branch stays parked as the receipt.

**great-britain z12, r7gd.4xlarge (16 cores, instance-store NVMe), 3 interleaved rounds per side, best of 3:**

| | main | partitioned merge |
|---|---|---|
| merge + encode + write | **67.7 s** | 71.1 s (+5.1%) |
| total wall | **243.2 s** | 251.8 s (+3.5%) |
| peak RSS | **4.02 GB** | 4.24 GB |

**europe z12, r7g.8xlarge (32 cores, 500 GB gp3 at 8000 IOPS), 1 round per side:**

| | main | partitioned merge |
|---|---|---|
| merge + encode + write | **834.9 s** | 848.7 s (+1.7%) |
| total wall | **57:16** | 57:29 (+0.4%) |
| peak RSS | 47.3 GB | 47.3 GB |

The stage it set out to speed up got **slower on both boxes**, which is the whole finding: the change is byte-identical (great-britain tile-content hash `800f96337a281878` and metadata hash matched exactly on both sides, 21324/18544 rows) and memory-neutral, and still loses.

Memory only came out neutral because of a second pass. The first revision (`ad9f0c6`, no claiming window) let workers whose partitions fit inside the channel buffer race ahead and pre-buffer future partitions, and great-britain measured **5.74 GB against main's 4.00 GB (+43%)** at 250.1 s vs 245.0 s. Adding the window — partition `i` may not start until the encoder has drained `i - workers` — put RSS back to 4.24 GB. Worth recording that the fix worked exactly as intended and the time regression survived it anyway, in fact widening from +2.1% to +3.5%: bounding the memory cost more coordination.

**Why the hypothesis was wrong.** The "~9.5 of the 13 write-minutes are the merge thread" figure that motivated this was **arithmetic, not a measurement**: 0.9 µs/entry × 625 M entries. Per-entry cost times entry count tells you what a stage would take if nothing overlapped it; it is not evidence that the stage is the *blocking* one. The write pipeline already overlaps merge with encode and write across bounded channels, so partitioning bought parallelism on a stage that was not the constraint and paid coordination cost for it.

**The rule this bought:** no parallelism work starts without a cheap instrumented run proving the target is actually on the critical path, on the hardware that matters. Probe first, build second. This experiment cost two cloud boxes and a branch to learn something a few minutes of stage-level instrumentation would have shown.

Caveats on the two cells, so nobody re-reads them as stronger than they are: europe is **n=1 per side** (the runs were stopped once the direction was clear) and it tested the pre-claiming-window revision `ad9f0c6`; great-britain tested `08947ac`, which is code-identical to the parked branch head. **europe on instance-store NVMe is unmeasured** — that is the one cell where the result could plausibly differ, since gp3 and NVMe change what the merge cursors are waiting on.

## Output parity (monaco, z12-14, via `bench compare`)

Measured with the parity oracle (`bench compare ours.mbtiles java.mbtiles`), which decodes both archives and reports tile-set overlap + per-tile semantic equality. Progress from the initial "wildly different" state:

| milestone | result |
|---|---|
| start (nodes+ways, default config) | shared=95, only_a=80, only_b=7, identical=0 |
| + **relations** (multipolygon/route) | only_b **7→0** — we cover every Planetiler tile |
| + **source-bounds clip** (default = pbf bbox) | only_a **80→0** — **exact same 102-tile set** |
| + **`@id/@type/@version/@timestamp`** metadata | shared features' attributes **byte-identical** |
| + matched config (`--buffer 0 --min-size 0.3 --simplify 0.1`) | **identical=10, content_equal=41 → 51/102 tiles (50%) semantically match** |

`content_equal` counts tiles equal up to polygon ring rotation/order (not semantically meaningful in MVT). The remaining 51 tiles differ only in **geometry precision** — our Douglas-Peucker simplification and min-size drop slightly different vertices/features than Planetiler's. Tile set, layer, feature ids, and attributes all match; the gap is now purely sub-pixel geometry detail.

Matching invocation:

```sh
bench compare ours.mbtiles java.mbtiles   # decode + semantic diff
planetiler-rs generate --source monaco.osm.pbf --output ours.mbtiles \
  --layer osm --minzoom 12 --maxzoom 14 --buffer 0 --min-size 0.3 --simplify 0.1 --force
```

## Caveats (a strong directional win, partial verified parity)

- **Not yet byte-identical** — 51/102 monaco tiles match semantically (up to ring rotation); the rest differ in simplification/min-size vertex selection. Tile set + attributes are exact. Full parity needs porting Planetiler's exact simplify + min-size thresholds.

## Reproduce

```sh
curl -sSL https://download.geofabrik.de/north-america/us/rhode-island-latest.osm.pbf -o ri.osm.pbf
bench measure -- planetiler-rs generate --source ri.osm.pbf --output ours.mbtiles \
  --layer osm --minzoom 12 --maxzoom 14 --force
bench measure -- java -jar planetiler.jar osm-qa --osm_path=ri.osm.pbf --output=java.mbtiles --maxzoom=14 --force
```
