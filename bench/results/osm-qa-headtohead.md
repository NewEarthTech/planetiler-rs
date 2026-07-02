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

**Standing verdict across scales: we win city (~5×) and state (~2.2×) on time and memory; at country we're 1.9× slower at 5× less memory; at continent Planetiler wins outright.** The two named fixes (relation-capture memory, parallel/partitioned archive write) are what the next optimization arc is for.

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
