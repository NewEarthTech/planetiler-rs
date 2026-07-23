# Is upstream Planetiler byte-deterministic?

The planetiler-rs pitch has three clauses -- faster, less memory, byte-identical -- and the third one only means something if the alternative is *not* byte-identical. planetiler-rs already carries cross-config byte-identity receipts (the same archive bytes across `--storage ram|mmap|sparse`, thread counts, and sort spill sizes; see the ladder in every perf PR and `osm-qa-headtohead.md`). This file measures the other side of the comparison: run pinned Planetiler v0.10.2 repeatedly, and across configurations, on great-britain, and hash the output. If Java is also deterministic, the "byte-identical" clause has to be stated far more precisely, and that is worth knowing before it goes in any pitch.

## Configuration

- Box: Apple M2 Max, 12 cores (8 performance + 4 efficiency), 96 GB, macOS 26.5.2.
- JVM: OpenJDK 26.0.1 (Homebrew). (The cloud head-to-head used OpenJDK 21; determinism here is an A/A on one machine + JVM, so the JVM version is held fixed and does not enter the comparison.)
- Jar: `parity/upstream/planetiler-v0.10.2.jar` (the pinned oracle), profile `osm-qa`.
- Input: `parity/data/great-britain.osm.pbf` (2.0 GB), z12 only.
- Invocation, identical to the cloud head-to-head (`parity/cloud/remote-planet.sh`), one output/tmpdir per run: `java -jar planetiler-v0.10.2.jar osm-qa --osm_path=great-britain.osm.pbf --output=<out> --tmpdir=<tmp> --minzoom=12 --maxzoom=12 --force`.
- Comparison, coarse to fine: L1 = sha256 of the whole `.mbtiles` file; L2 = sha256 of the ordered `(z,x,y,tile_data)` tile blobs; L3 = sha256 of the tile payloads after gunzip (Planetiler gzips MVT tiles, so this strips the container); plus the `metadata` table and the `tiles`/`tiles_data` row counts. Every hash below is the leading 16 hex chars.

## Result 1 -- run-to-run: Planetiler is byte-deterministic

Two runs, same input, same config, same machine. All 21,340 tiles, both archives exactly 1,547,767,808 bytes.

| level | run 1 | run 2 | verdict |
|---|---|---|---|
| L1 whole-file sha256 | `2b31706f26b4c9c6` | `2b31706f26b4c9c6` | **byte-identical** |
| L2 tile blobs (gzipped) | `51817e548937ec18` | `51817e548937ec18` | identical |
| L3 tile payloads (gunzipped MVT) | `5b31b3f33d74e957` | `5b31b3f33d74e957` | identical |
| metadata table | identical | | identical |
| tiles / tiles_data rows | 21340 / 21340 | 21340 / 21340 | match |

**The entire SQLite file is byte-for-byte identical between the two runs** -- not just the map data but the container, the metadata, and the gzip streams. The tiles are gzip-compressed (`1F8B` magic) and *still* match bit-for-bit, so Planetiler writes reproducible gzip (fixed mtime, no wall-clock in the header). Planetiler's `metadata` carries no run-varying timestamp that survives to disk here.

**So run-to-run determinism is not a planetiler-rs-only property.** On identical input + config + machine, upstream already gives you byte-identical archives. Any pitch that implies "Java can't reproduce its own output" is wrong and should be corrected.

## Result 2 -- config-invariance: the actual question

planetiler-rs's real determinism claim is stronger than run-to-run: the *same bytes fall out regardless of configuration* -- `--storage ram|mmap|sparse`, thread count, and sort-spill size are all byte-identical to each other. The apples-to-apples Java analogues are `--threads` (rs thread count) and `--nodemap_type` / `--nodemap_storage` (rs `--storage`). Run 1 above is the baseline (`--threads=12`, `--nodemap_type=sparsearray`).

All 21,340 tiles each; every archive exactly 1,547,767,808 bytes.

| run | change vs baseline | L1 whole-file sha256 | vs run 1 |
|---|---|---|---|
| baseline | `--threads=12 --nodemap_type=sparsearray` | `2b31706f26b4c9c6` | -- |
| threads=4 | `--threads=4` | `2b31706f26b4c9c6` | **identical** |
| nodemap=array | `--nodemap_type=array` | `2b31706f26b4c9c6` | **identical** |

**Planetiler's whole-file output does not move with thread count or node-map backend.** `--nodemap_type=array` swaps the entire node-location store (a dense array instead of the sparse run index) and the archive is still bit-for-bit identical, down to the SQLite page layout. Upstream evidently sorts features into a canonical order before the write stage, and its gzip + SQLite writer are reproducible, so configuration does not leak into the bytes.

## What this means for the "byte-identical" clause

**On this evidence, byte-identity is not a planetiler-rs-vs-Planetiler differentiator.** Both tools produce bit-for-bit identical archives run-to-run, and both hold that across the configuration knobs tested (thread count either side; nodemap backend on the Java side; storage mode / thread count / spill size on the rs side). The third clause of the pitch cannot be "byte-identical -- what Java can't do," because Java does it too.

- **Drop or restate the clause.** The defensible statement is that planetiler-rs is *itself* deterministic and config-invariant -- one archive, bit-for-bit, across `--storage ram|mmap|sparse`, thread counts, and spill sizes (the receipts in every perf PR). That is a real maintainability/testability property: any config is a valid oracle for any other, which is how this repo's own byte-identity ladder works. It is a property rs *shares with upstream*, not one it wins on.
- **What is left of the three clauses,** stated honestly against the planet head-to-head (`planet-headtohead.md`): *speed* is Java's at planet scale (4.5x) and rs's at city/state scale; *memory* is an rs win at small/mid scale and a wash at planet pending the constrained-memory A/B; *byte-identity* is a tie. The live, winnable ground is the scheduling gap the planet run exposed (pass 2 / merge feed at ~12-20% CPU), not determinism.
- **Caveat on scope.** This is one input (great-britain), one machine + JVM, at z12. It does not test determinism across *machines* with different core counts, across JVM versions, or at other zooms; a cross-machine difference is the one place Java's bytes could still move where rs's would not. That is a cheap future check (two boxes, or a capped-CPU cgroup), but nothing here hints Java is fragile, and the burden is on finding an asymmetry, not assuming one.

Raw: the archives (1.5 GB each) were built under `scratchpad`, not checked in; the invocations and the single recurring hash `2b31706f26b4c9c6` are the record.
