# Head-to-head: OSM QA tiles (planetiler-rs vs Planetiler v0.10.2)

Task: read an `.osm.pbf` extract and write all OSM features into a single `osm` layer as MBTiles, at the `osm-qa` zoom range **z12–14**. Measured with `bench measure` (`/usr/bin/time`) on a 12-core macOS machine (OpenJDK 26), default gzip tiles. Both tools at default settings (no heap tuning, no thread pinning).

## Results across two extract sizes

| extract | tool | wall | peak RSS | tiles |
|---------|------|-----:|---------:|------:|
| **monaco** (0.7 MB) | planetiler-rs | **0.089 s** | **69 MB** | 175 |
| | Planetiler | 2.20 s | 1068 MB | 102 |
| **rhode-island** (49 MB) | planetiler-rs | **4.21 s** | **680 MB** | 7522 |
| | Planetiler | 8.94 s | 7814 MB | 2203 |

**Speed:** ~25× faster on monaco → **~2.1× faster** on rhode-island. **Memory:** ~15× less on monaco → **~11.5× less** on rhode-island.

## Reading the trend (honest)

- **The time margin compresses with size** (25× → 2.1×) because monaco is so small that JVM cold-start (~1.5 s) dominates Planetiler's wall time. On rhode-island Java warms up and parallelizes, so ~2× is closer to the real steady-state speed advantage. We are still faster — but the honest headline speed number is ~2×, not ~25×.
- **The memory margin holds and widens** (~11.5× at rhode-island) — the core reason for the rewrite (no GC, no reserved JVM heap), and it survives warm-up. Planetiler even *failed* to run monaco under `-Xmx512m` / `-Xmx256m`; it needs a large default heap.
- **Our absolute memory now scales sub-linearly** (69 MB → 680 MB, ~14× the 49 MB input, down from ~25×) after the reader was made **streaming** and the node store **compacted** (fixed-point, 16 B/node): we no longer hold all features at once. Two in-RAM structures remain — the node-location store and the sort buffer (both bounded/tunable, both mmap-able) — so full planet scale still needs the off-heap `sparsearray` node store, but the linear-in-feature-count ceiling is gone.

## Caveats (a strong directional win, not verified parity)

- **Outputs are not identical** (rhode-island: 7522 vs 2203 tiles). We render **nodes + ways** but not **relations** yet, and our min-size / simplify / buffer differ from `osm-qa`'s config. We produce *more* tiles for less time and memory — but it is not the same output. Verified apples-to-apples needs relations + matched config + the semantic-MVT diff.

## Reproduce

```sh
curl -sSL https://download.geofabrik.de/north-america/us/rhode-island-latest.osm.pbf -o ri.osm.pbf
bench measure -- planetiler-rs generate --source ri.osm.pbf --output ours.mbtiles \
  --layer osm --minzoom 12 --maxzoom 14 --force
bench measure -- java -jar planetiler.jar osm-qa --osm_path=ri.osm.pbf --output=java.mbtiles --maxzoom=14 --force
```
