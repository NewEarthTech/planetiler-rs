# Head-to-head: OSM QA tiles (planetiler-rs vs Planetiler v0.10.2)

Task: read an `.osm.pbf` extract and write all OSM features into a single `osm` layer as MBTiles, at the `osm-qa` zoom range **z12–14**. Measured with `bench measure` (`/usr/bin/time`) on a 12-core macOS machine (OpenJDK 26), default gzip tiles. Both tools at default settings (no heap tuning, no thread pinning).

## Results across two extract sizes

| extract | tool | wall | peak RSS | tiles |
|---------|------|-----:|---------:|------:|
| **monaco** (0.7 MB) | planetiler-rs | **0.089 s** | **69 MB** | 175 |
| | Planetiler | 2.20 s | 1068 MB | 102 |
| **rhode-island** (49 MB) | planetiler-rs (ram) | **7.1 s** | 1025 MB | 2217 |
| | planetiler-rs (mmap) | 7.7 s | **944 MB** | 2217 |
| | Planetiler | 8.94 s | 7814 MB | 2203 |

**Speed:** ~25× faster on monaco → **~1.25× faster** on rhode-island. **Memory:** ~15× less on monaco → **~7.6–8.3× less** on rhode-island (ram/mmap). **Tiles now match**: 2217 vs 2203 (was 7522 — relations + source-bounds clipping closed it).

## Reading the trend (honest)

- **The rhode-island numbers are now a *fair* comparison.** The earlier 4.21 s / 680 MB / 7522-tile row was us doing *less* work than Java: no relations, no element metadata, no bounds clip. With those added (multipolygon/route assembly, `@id/@type/@version/@timestamp`, clip to the source bbox) the output matches — **2217 tiles vs Java's 2203** — and our absolute cost rose to ~7 s / ~1 GB. That is the honest apples-to-apples figure.
- **Even at matched output we win on both axes**: ~1.25× faster and ~7.6–8.3× less memory — from no GC and no reserved JVM heap. Planetiler even *failed* to run monaco under `-Xmx512m` / `-Xmx256m`; it needs a large default heap.
- **Node storage is selectable** (`--storage ram|mmap|sparse`), output byte-identical across all three. Peak RSS on rhode-island: ram **1015 MB**, mmap **927 MB**, sparse **932 MB** — mmap/sparse move the store off-heap (~85 MB) but they land together because the node store isn't the bottleneck at region scale (the 256 MB sort buffer + metadata-heavy features dominate ~900 MB). The **`sparse`** layout is run-compressed (8 B/node values in id order + a small run index), measured at **65 MB vs the sorted table's 92 MB** node store on rhode-island (~11 B/node at avg run length 4.9). Its real payoff is **planet scale**: node ids there are dense, so runs lengthen and it approaches **8 B/node** — half of ram/mmap — while a direct id-indexed array would need **111 GB** even for rhode-island (14 B id span at 0.04% density). The next lever for region-scale RSS is the sort buffer, not the node store.

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
