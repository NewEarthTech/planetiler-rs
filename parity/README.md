# Parity corpus & head-to-head benchmarking

Everything needed to prove planetiler-rs matches **upstream Planetiler** and to run fair time/memory comparisons against it. This directory holds the pinned upstream version, the input-download scripts, and the exact upstream commands used to regenerate reference tiles.

## Pinned upstream

- **Version:** Planetiler **v0.10.2** (released 2026-03-29). Pinning matters because tile byte-stability changed across releases; assert byte-identity only after confirming the pinned build is deterministic, otherwise assert semantic (decoded) equality.
- **Get the jar:** `PLANETILER_VERSION=v0.10.2 ./fetch-upstream.sh` downloads `upstream/planetiler-v0.10.2.jar` and verifies its published SHA-256.
- **Or Docker:** `docker run -e JAVA_TOOL_OPTIONS=-Xmx<N> ghcr.io/onthegomap/planetiler:v0.10.2 ...` (measure the container).

`upstream/` and `data/` are git-ignored (large binaries / downloaded extracts); the scripts and reference commands are tracked.

## Fair head-to-head rules

These are load-bearing; an unfair benchmark is worse than none. Full rationale in `ARCHITECTURE.md`.

1. **Pre-download once**, never time the download (`--only-download --area=monaco`).
2. **Pin identical knobs on both sides:** `--threads=<T>`, JVM `-Xmx<N>`, storage/nodemap mode, and `--tile-compression`.
3. **Capture at the OS level:** `/usr/bin/time -l` (macOS) / `-v` (Linux) for peak RSS; `bench measure -- <cmd>` wraps this.
4. **Discard a JVM warm-up run** (or annotate the cold-start penalty).
5. **Report `mmap` and `ram` storage separately**: mmap hides node-store bytes from RSS and skews the memory comparison.
6. **Compare decompressed tile payloads** (upstream gzips + dedups by default), and diff semantically unless byte-determinism is confirmed on the pinned version.

## Dedicated cloud runs (`services/cloud-runner/`)

The laptop is a shared development machine, and a few-percent effect is invisible under its noise floor (`bench/results/` records a 14% spread on identical binaries between sessions). `services/cloud-runner/run.sh` exists for measurements that need a quiet box: it launches a dedicated EC2 instance, runs a job on it, fetches the results, and terminates it.

```sh
services/cloud-runner/run.sh <job> <ref-a> <ref-b> [rounds] [instance-type] [max-hours] [data-ebs-gb]
services/cloud-runner/run.sh gb-ab main perf/some-branch
services/cloud-runner/run.sh europe-ab main perf/some-branch 2 r7g.8xlarge 8 500
```

Jobs are `gb-ab` and `europe-ab`; both interleave rounds of ref-a against ref-b on one box and prove the two outputs match (a full content hash on great-britain, a per-tile signature on europe, where hashing 24 GB of tiles twice would dominate the run). `redispatch.sh <ip> ...` reuses a box that was kept alive; `remote.sh` is what executes on the instance.

Operational notes that are load-bearing:

- **One box per dataset, both refs on it.** Never split ref-a and ref-b across instances; you would be measuring the instances.
- **`d`-suffix instance types** (`r7gd`, `i4g`) use local NVMe for `/data`. Types without instance store need `data-ebs-gb`, which provisions gp3 at 8000 IOPS / 500 MB/s. This matters more than it sounds: the europe run on gp3 spent 2601 s in stream/render/sort-feed against roughly 1410 s for the same stage on the laptop's local NVMe. **That run was disk-bound, and its wall time should not be read as a CPU result.**
- **`TMPDIR` is pointed at `/data`.** All four scratch consumers (sort spill, node store, way-geometry store, PMTiles staging) use `tempfile`, which honours it. Left unset, hundreds of GB would land on the 30 GB root volume.
- **Safety:** the instance schedules `shutdown -h +<max-hours>` at boot and shutdown means terminate, so a wedged run cannot idle for a weekend. SSH is opened to the launching IP only, and the repo travels as a git bundle so no credentials leave the laptop.
- `services/cloud-runner/results/` is git-ignored: those are per-run artifacts, and the numbers worth keeping belong in `bench/results/`.

## Regenerating reference tiles (example, once implemented)

```sh
# monaco, single-threaded, 4 GiB heap, gzip tiles; pin the same knobs for planetiler-rs.
java -Xmx4g -jar upstream/planetiler-v0.10.2.jar \
  --area=monaco --osm-path=data/monaco.osm.pbf \
  --threads=1 --tile-compression=gzip \
  --output=data/monaco.upstream.mbtiles --force
```

The macro runner (`src/bin/bench.rs`) will drive both sides, diff output, and write a comparison row under `../bench/results/`.
