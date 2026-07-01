# Head-to-head: monaco OSM QA tiles (planetiler-rs vs Planetiler v0.10.2)

First real OSM benchmark. Task: read `monaco.osm.pbf` (669 KB, Geofabrik) and write all OSM features into a single `osm` layer as an MBTiles archive. Measured with `bench measure` (`/usr/bin/time -l`) on a 12-core machine (macOS, OpenJDK 26). Both default gzip tile compression.

## Results (matched zoom range z12–14, Planetiler `osm-qa`'s default)

| tool | wall | peak RSS | tiles |
|------|-----:|---------:|------:|
| **planetiler-rs** | **0.089 s** | **69 MB** | 175 |
| Planetiler v0.10.2 `osm-qa` | 2.20 s | 1068 MB | 102 |

**≈25× faster, ≈15× less memory.** planetiler-rs also completed comfortably where Planetiler **failed under a capped heap**: `-Xmx512m` and `-Xmx256m` both exited non-zero (default heap RSS ≈1 GB for this tiny extract).

Full-range z0–14 (planetiler-rs only, for reference): 0.15–0.44 s, ≈137 MB, 199 tiles.

## Honest caveats (this is a strong directional result, not yet verified parity)

- **Outputs are not identical.** planetiler-rs emits 175 tiles at z12–14 vs Planetiler's 102. We render **nodes + ways** but **not relations yet**, and our min-size / simplification / buffer differ from `osm-qa`'s exact configuration. So this is "comparable task, dramatically faster and lighter" — not "byte-identical output, Nx faster." Closing this gap (relations + matching min-size/simplify) is the next step for a verified apples-to-apples number.
- **JVM startup inflates Planetiler's time** on an extract this tiny (~1.5 s cold-start dominates its 2.2 s). The time ratio will shrink on larger inputs; the **memory advantage (~15×) is startup-independent** and is the core reason for the rewrite.
- Monaco is a tiny extract; the real test is region/planet scale (future).

## Reproduce

```sh
# input
curl -sSL https://download.geofabrik.de/europe/monaco-latest.osm.pbf -o monaco.osm.pbf

# planetiler-rs (z12-14 to match osm-qa)
bench measure -- planetiler-rs generate --source monaco.osm.pbf --output ours.mbtiles \
  --layer osm --minzoom 12 --maxzoom 14 --force

# Planetiler v0.10.2
bench measure -- java -jar planetiler.jar osm-qa \
  --osm_path=monaco.osm.pbf --output=java.mbtiles --maxzoom=14 --force
```
