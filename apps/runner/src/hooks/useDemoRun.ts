import { useEffect, useRef, useState } from "react";
import fixtureRaw from "@/fixtures/planet-run-20260722.log?raw";
import { EMPTY_FEED, type Feed, nextFeed, type RunState } from "@/lib/feed";
import { type LogLine, parseLog } from "@/lib/log";

const DEMO_LINES = parseLog(fixtureRaw);
/** Real 10 526 s replayed in ~88 s. */
const SPEED = 120;
const TICK_MS = 100;
const PROVISION_MS = 2500;

export interface DemoRun extends Feed {
  state: RunState;
  requisition: () => void;
  dispatch: () => void;
  cancel: () => void;
  destroy: () => void;
}

/**
 * The demo replay: a client-only state machine that streams the checked-in
 * planet-run fixture through the same shapes a live run uses. No backend,
 * no network — Dispatch replays, Cancel freezes, Destroy resets.
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
