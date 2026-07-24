import type { LogLine } from "@/lib/log";
import { PHASES } from "@/lib/phases";

/** Demo state machine states; the live view maps onto the subset it needs. */
export type RunState = "idle" | "provisioning" | "ready" | "running" | "done" | "cancelled";

export interface Counters {
  pieces: number | null;
  entries: number | null;
  tiles: number | null;
}

/**
 * The rolling view a run feed maintains: the visible log tail, elapsed time,
 * per-phase completion (from the marker lines), and headline counters. The
 * demo replay and the live CloudWatch tail both reduce into this shape.
 */
export interface Feed {
  lines: LogLine[];
  simElapsedS: number;
  /** Real per-phase elapsed_s, recorded when the phase's marker line is emitted. */
  phaseDone: Record<string, number>;
  counters: Counters;
}

export const MAX_LINES = 500;

export const EMPTY_FEED: Feed = {
  lines: [],
  simElapsedS: 0,
  phaseDone: {},
  counters: { pieces: null, entries: null, tiles: null },
};

export function nextFeed(prev: Feed, appended: LogLine[], simElapsedS: number): Feed {
  const lines = appended.length ? [...prev.lines, ...appended].slice(-MAX_LINES) : prev.lines;
  let phaseDone = prev.phaseDone;
  let counters = prev.counters;
  for (const line of appended) {
    for (const phase of PHASES) {
      if (phase.donePattern.test(line.message)) {
        phaseDone = { ...phaseDone, [phase.id]: Number(line.fields.elapsed_s) };
      }
    }
    const pieces = line.fields.total_pieces;
    const entries = line.fields.entries_consumed;
    const tiles = line.fields.tiles_completed ?? line.fields.tiles;
    if (pieces || entries || tiles) {
      counters = {
        pieces: pieces ? Number(pieces) : counters.pieces,
        entries: entries ? Number(entries) : counters.entries,
        tiles: tiles ? Number(tiles) : counters.tiles,
      };
    }
  }
  return { lines, simElapsedS, phaseDone, counters };
}
