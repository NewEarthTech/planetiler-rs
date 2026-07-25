import type { CloudWatchLogsClient } from "@aws-sdk/client-cloudwatch-logs";
import { useEffect, useState } from "react";
import { EMPTY_FEED, type Feed, nextFeed } from "@/lib/feed";
import { createLogParser, type LogLine } from "@/lib/log";
import type { PrepareState } from "@/lib/phases";
import {
  LIVE_WINDOW_MS,
  type MemSample,
  parseMemSample,
  type RunSummary,
  readForward,
  readLatest,
} from "@/lib/runs";

const TAIL_POLL_MS = 3_000;
const MEM_POLL_MS = 10_000;
/** 40 pages × 10k events per tick — a whole planet stderr catches up in one go. */
const MAX_PAGES_PER_TICK = 40;

export type LiveStatus = "connecting" | "live" | "done" | "stale";

export interface LiveRun {
  feed: Feed;
  status: LiveStatus;
  /** The stream feeding the log window, e.g. "planet-20260722-031500/planetiler". */
  streamName: string;
  /** The pre-pipeline stage (deps + input download); null for non-planet runs. */
  prepare: PrepareState | null;
  mem: MemSample | null;
  error: string | null;
}

/** Planet runs spend their first ~quarter hour downloading the input. */
export function hasPrepareStage(runId: string): boolean {
  return runId.startsWith("planet-");
}

/**
 * Tail one run out of CloudWatch Logs into the shared feed shape: read the
 * main stream oldest-first (so phase markers and counters replay correctly),
 * then keep following its forward token. Prefers the planetiler stderr stream
 * and falls back to job.log until that stream exists; the switch resets the
 * feed and re-reads from the head.
 */
export function useLiveRun(
  client: CloudWatchLogsClient,
  logGroup: string,
  run: RunSummary,
): LiveRun {
  const suffix = run.suffixes.includes("planetiler") ? "planetiler" : "job";
  const streamName = `${run.id}/${suffix}`;
  const hasMem = run.suffixes.includes("mem");
  const memStream = `${run.id}/mem`;

  const [feed, setFeed] = useState<Feed>(EMPTY_FEED);
  const [status, setStatus] = useState<LiveStatus>("connecting");
  const [mem, setMem] = useState<MemSample | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let inFlight = false;
    let token: string | undefined;
    let lastEventAt = 0;
    let current = EMPTY_FEED;
    const parse = createLogParser();
    setFeed(EMPTY_FEED);
    setStatus("connecting");
    setError(null);

    const tick = async () => {
      if (inFlight || cancelled) return;
      inFlight = true;
      try {
        const appended: LogLine[] = [];
        for (let page = 0; page < MAX_PAGES_PER_TICK; page++) {
          const res = await readForward(client, logGroup, streamName, token);
          token = res.nextToken;
          for (const event of res.events) {
            const line = parse(event.message);
            if (line) appended.push(line);
            if (event.timestamp != null && event.timestamp > lastEventAt) {
              lastEventAt = event.timestamp;
            }
          }
          if (res.caughtUp) break;
        }
        if (cancelled) return;
        current = nextFeed(current, appended, appended.at(-1)?.offsetS ?? current.simElapsedS);
        setFeed(current);
        const done = current.phaseDone.merge != null;
        setStatus(done ? "done" : Date.now() - lastEventAt < LIVE_WINDOW_MS ? "live" : "stale");
        setError(null);
      } catch (err) {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      } finally {
        inFlight = false;
      }
    };

    void tick();
    const timer = setInterval(() => void tick(), TAIL_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [client, logGroup, streamName]);

  useEffect(() => {
    setMem(null);
    if (!hasMem) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const messages = await readLatest(client, logGroup, memStream, 10);
        if (!cancelled) setMem(parseMemSample(messages));
      } catch {
        // The mem readout is decoration; the main tail surfaces real errors.
      }
    };
    void tick();
    const timer = setInterval(() => void tick(), MEM_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [client, logGroup, memStream, hasMem]);

  // This polling path can't cheaply time the stage (no cross-stream reads),
  // but it can stop lying: the planetiler stream existing at all is what
  // proves the binary started, so until then the panel shows prepare, not
  // "pass 1 · nodes".
  const hasBinaryStream = run.suffixes.includes("planetiler");
  const prepare: PrepareState | null = hasPrepareStage(run.id)
    ? {
        done: hasBinaryStream,
        active: !hasBinaryStream && (status === "live" || status === "connecting"),
        doneS: null,
        elapsedS: null,
      }
    : null;

  return { feed, status, streamName, prepare, mem, error };
}
