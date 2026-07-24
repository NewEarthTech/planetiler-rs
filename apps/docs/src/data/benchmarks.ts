/**
 * Benchmark dataset, transcribed from planetiler-rs `bench/results/` (laptop
 * history, committed) and the EC2 runner's fetched logs (`services/cloud-runner/results/`,
 * gitignored run artifacts — curated numbers land in `bench/results/`).
 * Task everywhere: OSM extract → osm-qa-style MBTiles (single `osm` layer),
 * z12 (country/continent) or z12–14 (city/state). Upstream pinned at v0.10.2.
 *
 * Cost = instance on-demand rate × wall clock (us-east-1). Laptop runs carry
 * no cost — the machine was already paid for and heavily contended, which is
 * exactly why the cloud runs exist.
 *
 * Every number here must trace to a recorded run. If you cannot point at the
 * line in `bench/results/` it came from, it does not belong on the page.
 */

export type Tool = "planetiler-rs" | "planetiler-java";
export type Machine = "laptop-12c" | "r7gd.4xlarge" | "r7g.8xlarge";

export const MACHINE_LABEL: Record<Machine, string> = {
  "laptop-12c": "laptop · 12c/96GB (Apple)",
  "r7gd.4xlarge": "EC2 r7gd.4xlarge · 16c/128GB",
  "r7g.8xlarge": "EC2 r7g.8xlarge · 32c/256GB",
};

/** us-east-1 on-demand $/hour; used to derive cost per build. */
export const HOURLY_RATE: Partial<Record<Machine, number>> = {
  "r7gd.4xlarge": 1.0906,
  "r7g.8xlarge": 1.7136,
};

export interface Extract {
  id: string;
  label: string;
  inputMB: number;
}

/** Ordered by input size — the x axis of the cross-scale charts. */
export const EXTRACTS: Extract[] = [
  { id: "monaco", label: "monaco", inputMB: 0.7 },
  { id: "rhode-island", label: "rhode-island", inputMB: 49 },
  { id: "great-britain", label: "great-britain", inputMB: 2043 },
  { id: "europe", label: "europe", inputMB: 34600 },
];

export interface Run {
  extract: string;
  tool: Tool;
  machine: Machine;
  /** Wall-clock seconds, best of interleaved rounds where rounds exist. */
  wallS: number;
  /** Peak resident set size, MB. */
  peakRssMB: number;
  date: string;
  note?: string;
}

/** Best recorded head-to-head runs per (extract, tool, machine). */
export const RUNS: Run[] = [
  // — laptop history (bench/results/osm-qa-headtohead.md) —
  { extract: "monaco", tool: "planetiler-rs", machine: "laptop-12c", wallS: 0.41, peakRssMB: 84, date: "2026-07-01" },
  { extract: "monaco", tool: "planetiler-java", machine: "laptop-12c", wallS: 2.2, peakRssMB: 1068, date: "2026-07-01" },
  { extract: "rhode-island", tool: "planetiler-rs", machine: "laptop-12c", wallS: 4.0, peakRssMB: 1088, date: "2026-07-01", note: "sparse node store" },
  { extract: "rhode-island", tool: "planetiler-java", machine: "laptop-12c", wallS: 8.94, peakRssMB: 7814, date: "2026-07-01" },
  { extract: "great-britain", tool: "planetiler-rs", machine: "laptop-12c", wallS: 153, peakRssMB: 5939, date: "2026-07-01", note: "sparse, z12" },
  { extract: "great-britain", tool: "planetiler-java", machine: "laptop-12c", wallS: 81, peakRssMB: 29798, date: "2026-07-01", note: "z12" },
  { extract: "europe", tool: "planetiler-rs", machine: "laptop-12c", wallS: 2496, peakRssMB: 54579, date: "2026-07-02", note: "round 2: parallel write + relation capture off heap" },
  { extract: "europe", tool: "planetiler-java", machine: "laptop-12c", wallS: 1062, peakRssMB: 76698, date: "2026-07-01", note: "stock defaults" },

  // — dedicated EC2 runs (services/cloud-runner/results/) —
  // Geofabrik re-fetched on the run date, so this is a later extract vintage
  // than the laptop row above (21324 tiles vs 21472) as well as different
  // hardware. Same flags, but not a like-for-like pair with the laptop number.
  { extract: "great-britain", tool: "planetiler-rs", machine: "r7gd.4xlarge", wallS: 245.0, peakRssMB: 4194, date: "2026-07-03", note: "best of 3 interleaved rounds, geofabrik-latest input" },
];

export interface TrajectoryPoint {
  extract: "great-britain" | "europe";
  round: number;
  label: string;
  wallS: number;
  date: string;
}

/**
 * planetiler-rs optimization trajectory on the laptop (same machine, same
 * input, byte-identical output at every step — pure performance history).
 */
export const TRAJECTORY: TrajectoryPoint[] = [
  { extract: "great-britain", round: 1, label: "baseline", wallS: 332, date: "2026-07-01" },
  { extract: "great-britain", round: 2, label: "relation fixes", wallS: 250, date: "2026-07-01" },
  { extract: "great-britain", round: 3, label: "parallel decode", wallS: 153, date: "2026-07-01" },
  // Round 4 comes from the parallel-write A/B, a different session from rounds
  // 1-3: main measured 173.8 s there, so the branch's own gain was -13%, not
  // the -1% the round-3 comparison suggests. Machine noise, not a plateau.
  { extract: "great-britain", round: 4, label: "parallel write", wallS: 150.9, date: "2026-07-02" },
  { extract: "europe", round: 1, label: "first run", wallS: 3198, date: "2026-07-01" },
  { extract: "europe", round: 2, label: "parallel write + relation memory", wallS: 2496, date: "2026-07-02" },
];

export function costUsd(run: Run): number | null {
  const rate = HOURLY_RATE[run.machine];
  return rate == null ? null : (run.wallS / 3600) * rate;
}

export const LAST_UPDATED = "2026-07-03";
