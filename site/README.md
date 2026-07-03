# tilebench

The benchmark scoreboard for [planetiler-rs](https://github.com/johncarmack1984/planetiler-rs) vs Java [Planetiler](https://github.com/onthegomap/planetiler), deployed at [tilebench.johncarmack.com](https://tilebench.johncarmack.com). Three questions, charted: how long does the build take, how much memory does it hold, and what does it cost on a dedicated cloud box.

## Where the numbers come from

Every point is a recorded run of the same task on both tools — OSM extract in, osm-qa-style MBTiles out, upstream pinned at v0.10.2, output verified byte-identical (or semantically equal cross-tool) before the number counts. Laptop history is transcribed from [`bench/results/`](../bench/results); cloud numbers come from [`parity/cloud/`](../parity/cloud) runs on dedicated EC2 instances (interleaved best-of-N rounds, one box per dataset). Data lives in `src/data/benchmarks.ts` — append new runs there.

## Stack

Vite + React 19 + Tailwind v4, shadcn-style components on [Base UI](https://base-ui.com) primitives, Recharts line charts. Palette and chart rules follow a validated colorblind-safe spec (light and dark are separately chosen steps, both pass CVD/contrast checks; charts ship legends, end-of-line labels, and a table view).

## Develop & deploy

```sh
bun install && bun run dev   # local
just deploy                  # build → S3 sync → CloudFront invalidation
```

`infra/` is a CDK app (S3 behind CloudFront OAC, DNS-validated ACM cert, Route53 aliases in the parent zone, BucketDeployment publishing `dist/`). One-time setup: copy `infra/.env.example` to `infra/.env` and fill in the parent hosted zone id; the account comes from your AWS credentials, nothing is baked in.
