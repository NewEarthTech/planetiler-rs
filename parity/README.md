# Parity corpus & head-to-head benchmarking

Everything needed to prove planetiler-rs matches **upstream Planetiler** and to run fair time/memory comparisons against it. This directory holds the pinned upstream version, the input-download scripts, and the exact upstream commands used to regenerate reference tiles.

## Pinned upstream

- **Version:** Planetiler **v0.10.2** (released 2026-03-29). Pinning matters because tile byte-stability changed across releases — assert byte-identity only after confirming the pinned build is deterministic, otherwise assert semantic (decoded) equality.
- **Get the jar:** `PLANETILER_VERSION=v0.10.2 ./fetch-upstream.sh` downloads `upstream/planetiler-v0.10.2.jar` and verifies its published SHA-256.
- **Or Docker:** `docker run -e JAVA_TOOL_OPTIONS=-Xmx<N> ghcr.io/onthegomap/planetiler:v0.10.2 ...` (measure the container).

`upstream/` and `data/` are git-ignored (large binaries / downloaded extracts); the scripts and reference commands are tracked.

## Fair head-to-head rules

These are load-bearing — an unfair benchmark is worse than none. Full rationale in `docs/ARCHITECTURE.md`.

1. **Pre-download once**, never time the download (`--only-download --area=monaco`).
2. **Pin identical knobs on both sides:** `--threads=<T>`, JVM `-Xmx<N>`, storage/nodemap mode, and `--tile-compression`.
3. **Capture at the OS level:** `/usr/bin/time -l` (macOS) / `-v` (Linux) for peak RSS; `bench measure -- <cmd>` wraps this.
4. **Discard a JVM warm-up run** (or annotate the cold-start penalty).
5. **Report `mmap` and `ram` storage separately** — mmap hides node-store bytes from RSS and skews the memory comparison.
6. **Compare decompressed tile payloads** (upstream gzips + dedups by default), and diff semantically unless byte-determinism is confirmed on the pinned version.

## Regenerating reference tiles (example, once implemented)

```sh
# monaco, single-threaded, 4 GiB heap, gzip tiles — pin the same knobs for planetiler-rs.
java -Xmx4g -jar upstream/planetiler-v0.10.2.jar \
  --area=monaco --osm-path=data/monaco.osm.pbf \
  --threads=1 --tile-compression=gzip \
  --output=data/monaco.upstream.mbtiles --force
```

The macro runner (`src/bin/bench.rs`) will drive both sides, diff output, and write a comparison row under `../bench/results/`.
