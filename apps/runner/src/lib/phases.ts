/**
 * Phase boundaries measured from the first full planet run
 * (bench/results/planet-run-20260722: planet-260713 on r7gd.8xlarge,
 * wall 10 526 s). The demo progress model and the stage rows key off
 * these offsets; a live run will close phases from its own marker lines.
 */
export interface Phase {
  id: string;
  label: string;
  /** Offset (s since run start) at which the phase ended in the reference run. */
  endS: number;
  /** Marker message that closes the phase (matched against LogLine.message). */
  donePattern: RegExp;
}

/**
 * The prepare stage that precedes the pipeline: deps, the input download
 * (94 GB for planet), the upstream jar. It has no reference width — the
 * PHASES offsets are measured from binary start — so it renders as a row,
 * not a bar segment. Done the moment the planetiler stream produces its
 * first line: the binary running IS the marker, no log grammar involved.
 * Null on the hooks' output means the run has no prepare stage to show.
 */
export interface PrepareState {
  done: boolean;
  /** Lines are flowing but the binary hasn't started: prepare is what's running. */
  active: boolean;
  /** Seconds from the first job-log line to the first binary line, once known. */
  doneS: number | null;
  /** Live counter while active; frozen to doneS after. */
  elapsedS: number | null;
}

export const PREPARE_LABEL = "prepare · input download";

export const PHASES: readonly Phase[] = [
  { id: "pass1", label: "pass 1 · nodes", endS: 1219.9, donePattern: /^osm pass 1 \(nodes\) done/ },
  { id: "pass2", label: "pass 2 · ways", endS: 5670.1, donePattern: /^osm pass 2 \(ways\) done/ },
  {
    id: "stream",
    label: "relations · render · sort",
    endS: 8116.1,
    donePattern: /^stream \+ render \+ sort-feed done/,
  },
  {
    id: "merge",
    label: "merge · encode · write",
    endS: 10526.2,
    donePattern: /^merge \+ encode \+ write done/,
  },
];

export const TOTAL_S = PHASES[PHASES.length - 1].endS;
