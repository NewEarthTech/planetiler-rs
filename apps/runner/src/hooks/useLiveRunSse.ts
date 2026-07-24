import { useEffect, useState } from "react";
import { EMPTY_FEED, type Feed, nextFeed } from "@/lib/feed";
import { type LiveConfig, streamRun } from "@/lib/live";
import { createLogParser, type LogLine } from "@/lib/log";
import { LIVE_WINDOW_MS, type MemSample, parseMemSample, type RunSummary } from "@/lib/runs";
import type { LiveRun, LiveStatus } from "@/hooks/useLiveRun";

/**
 * Batch UI updates: events arrive per line over the socket (a backfill can
 * be a couple hundred thousand of them), so state flushes on a short tick
 * instead of per event. 150 ms is invisible next to the transport.
 */
const FLUSH_MS = 150;

/**
 * The SSE flavor of useLiveRun: same contract, but fed by one long-lived
 * stream from the runner-live backend instead of GetLogEvents polling — the
 * mem readout rides the same stream, so the separate 10 s poll disappears
 * too. Reconnects resume from the last seq; CloudWatch never enters the
 * picture here.
 */
export function useLiveRunSse(config: LiveConfig, idToken: string, run: RunSummary): LiveRun {
  const suffix = run.suffixes.includes("planetiler") ? "planetiler" : "job";
  const streamName = `${run.id}/${suffix}`;

  const [feed, setFeed] = useState<Feed>(EMPTY_FEED);
  const [status, setStatus] = useState<LiveStatus>("connecting");
  const [mem, setMem] = useState<MemSample | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const abort = new AbortController();
    const parse = createLogParser();
    let current = EMPTY_FEED;
    let pending: LogLine[] = [];
    let memLatest: MemSample | null = null;
    let memDirty = false;
    let lastEventAt = 0;
    let connected = false;
    setFeed(EMPTY_FEED);
    setStatus("connecting");
    setMem(null);
    setError(null);

    const flush = () => {
      if (pending.length) {
        current = nextFeed(current, pending, pending.at(-1)?.offsetS ?? current.simElapsedS);
        pending = [];
        setFeed(current);
      }
      if (memDirty) {
        memDirty = false;
        setMem(memLatest);
      }
      const done = current.phaseDone.merge != null;
      setStatus(
        done
          ? "done"
          : !connected
            ? "connecting"
            : lastEventAt && Date.now() - lastEventAt < LIVE_WINDOW_MS
              ? "live"
              : "stale",
      );
    };

    streamRun(config, run.id, idToken, abort.signal, {
      onEvent: (event) => {
        if (event.ts > lastEventAt) lastEventAt = event.ts;
        if (event.stream === suffix) {
          const line = parse(event.text);
          if (line) pending.push(line);
        } else if (event.stream === "mem") {
          const sample = parseMemSample([event.text]);
          if (sample) {
            memLatest = sample;
            memDirty = true;
          }
        }
      },
      onConnected: (next) => {
        connected = next;
        if (next) setError(null);
      },
    }).catch((err: unknown) => {
      // streamRun only returns on abort; this is a belt for the unexpected.
      if (!abort.signal.aborted) setError(err instanceof Error ? err.message : String(err));
    });

    const timer = setInterval(flush, FLUSH_MS);
    return () => {
      abort.abort();
      clearInterval(timer);
    };
  }, [config, idToken, run.id, suffix]);

  return { feed, status, streamName, mem, error };
}
