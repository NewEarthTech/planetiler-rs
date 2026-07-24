import { useEffect, useRef, useState } from "react";
import fixtureRaw from "@/fixtures/planet-run-20260722.log?raw";
import { type LogLine, parseLog } from "@/lib/log";
import { PHASES } from "@/lib/phases";

const DEMO_LINES = parseLog(fixtureRaw);
/** Real 10 526 s replayed in ~88 s. */
const SPEED = 120;
const TICK_MS = 100;
const PROVISION_MS = 2500;
const MAX_LINES = 500;

export type RunState = "idle" | "provisioning" | "ready" | "running" | "done" | "cancelled";

export interface Counters {
  pieces: number | null;
  entries: number | null;
  tiles: number | null;
}

interface Feed {
  lines: LogLine[];
  simElapsedS: number;
  /** Real per-phase elapsed_s, recorded when the phase's marker line is emitted. */
  phaseDone: Record<string, number>;
  counters: Counters;
}

export interface DemoRun extends Feed {
  state: RunState;
  requisition: () => void;
  dispatch: () => void;
  cancel: () => void;
  destroy: () => void;
}

const EMPTY_FEED: Feed = {
  lines: [],
  simElapsedS: 0,
  phaseDone: {},
  counters: { pieces: null, entries: null, tiles: null },
};

function nextFeed(prev: Feed, appended: LogLine[], simElapsedS: number): Feed {
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

/**
 * The demo replay: a client-only state machine that streams the checked-in
 * planet-run fixture through the same shapes a live run will use. No
 * backend, no network — Dispatch replays, Cancel freezes, Destroy resets.
 */
export function useDemoRun(): DemoRun {
  const [state, setState] = useState<RunState>("idle");
  const [feed, setFeed] = useState<Feed>(EMPTY_FEED);
  const t0Ref = useRef(0);
  const cursorRef = useRef(0);

  // Provisioning is a fixed-delay hop to "ready" (no box exists in demo mode).
  useEffect(() => {
    if (state !== "provisioning") return;
    const timer = setTimeout(() => setState("ready"), PROVISION_MS);
    return () => clearTimeout(timer);
  }, [state]);

  // Replay: every tick, append the fixture lines whose offsets have been reached.
  useEffect(() => {
    if (state !== "running") return;
    const timer = setInterval(() => {
      const simElapsedS = ((performance.now() - t0Ref.current) / 1000) * SPEED;
      const appended: LogLine[] = [];
      while (
        cursorRef.current < DEMO_LINES.length &&
        DEMO_LINES[cursorRef.current].offsetS <= simElapsedS
      ) {
        appended.push(DEMO_LINES[cursorRef.current]);
        cursorRef.current += 1;
      }
      setFeed((prev) => nextFeed(prev, appended, simElapsedS));
      if (cursorRef.current >= DEMO_LINES.length) setState("done");
    }, TICK_MS);
    return () => clearInterval(timer);
  }, [state]);

  const requisition = () => setState("provisioning");
  const dispatch = () => {
    t0Ref.current = performance.now();
    cursorRef.current = 0;
    setFeed(EMPTY_FEED);
    setState("running");
  };
  const cancel = () => setState("cancelled");
  const destroy = () => {
    setFeed(EMPTY_FEED);
    setState("idle");
  };

  return { state, ...feed, requisition, dispatch, cancel, destroy };
}
