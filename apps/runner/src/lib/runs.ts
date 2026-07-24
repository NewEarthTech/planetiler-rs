// Read side of the live log drain: the signed-in SPA trades its Cognito id
// token for identity-pool credentials scoped to the run log group, then polls
// CloudWatch Logs directly. No server in between — the boxes write to the
// group (services/cloud-runner/drain.sh), the browser reads from it.

import {
  CloudWatchLogsClient,
  DescribeLogStreamsCommand,
  GetLogEventsCommand,
} from "@aws-sdk/client-cloudwatch-logs";
import { fromCognitoIdentityPool } from "@aws-sdk/credential-providers";
import type { RunsConfig } from "@/lib/auth";

/** Stream naming contract with drain.sh: `<run-id>/<suffix>`. */
export const STREAM_SUFFIXES = ["job", "planetiler", "mem"] as const;
export type StreamSuffix = (typeof STREAM_SUFFIXES)[number];

/** A run with events this recent is considered in flight. */
export const LIVE_WINDOW_MS = 3 * 60_000;

export interface RunSummary {
  /** e.g. "planet-20260722-031500" — job name + dispatch stamp. */
  id: string;
  /** Which of the per-run streams exist so far. */
  suffixes: StreamSuffix[];
  firstEventAt: number | null;
  lastEventAt: number | null;
}

export function makeRunsClient(
  runs: RunsConfig,
  userPoolProvider: string,
  idToken: string,
): CloudWatchLogsClient {
  return new CloudWatchLogsClient({
    region: runs.region,
    credentials: fromCognitoIdentityPool({
      identityPoolId: runs.identityPoolId,
      logins: { [userPoolProvider]: idToken },
      clientConfig: { region: runs.region },
    }),
  });
}

export function isLive(run: RunSummary): boolean {
  return run.lastEventAt !== null && Date.now() - run.lastEventAt < LIVE_WINDOW_MS;
}

/** The runs the log group knows about, most recently active first. */
export async function listRuns(
  client: CloudWatchLogsClient,
  logGroup: string,
): Promise<RunSummary[]> {
  const res = await client.send(
    new DescribeLogStreamsCommand({
      logGroupName: logGroup,
      orderBy: "LastEventTime",
      descending: true,
      limit: 50,
    }),
  );
  const byId = new Map<string, RunSummary>();
  for (const stream of res.logStreams ?? []) {
    const name = stream.logStreamName ?? "";
    const slash = name.lastIndexOf("/");
    if (slash <= 0) continue;
    const id = name.slice(0, slash);
    const suffix = name.slice(slash + 1) as StreamSuffix;
    if (!STREAM_SUFFIXES.includes(suffix)) continue;
    const run = byId.get(id) ?? { id, suffixes: [], firstEventAt: null, lastEventAt: null };
    run.suffixes.push(suffix);
    run.firstEventAt = minOf(run.firstEventAt, stream.firstEventTimestamp);
    run.lastEventAt = maxOf(run.lastEventAt, stream.lastEventTimestamp);
    byId.set(id, run);
  }
  return [...byId.values()].sort((a, b) => (b.lastEventAt ?? 0) - (a.lastEventAt ?? 0));
}

export interface LogEvent {
  message: string;
  /** Ingestion-side event timestamp (ms) — drives the live/stale call. */
  timestamp: number | undefined;
}

export interface LogPage {
  events: LogEvent[];
  nextToken: string | undefined;
  /** True when this page reached the current end of the stream. */
  caughtUp: boolean;
}

/** Read a stream oldest-first; pass the previous page's token to continue. */
export async function readForward(
  client: CloudWatchLogsClient,
  logGroup: string,
  stream: string,
  nextToken: string | undefined,
): Promise<LogPage> {
  const res = await client.send(
    new GetLogEventsCommand({
      logGroupName: logGroup,
      logStreamName: stream,
      startFromHead: true,
      nextToken,
      limit: 10_000,
    }),
  );
  const events = (res.events ?? []).flatMap((e) =>
    e.message != null ? [{ message: e.message, timestamp: e.timestamp }] : [],
  );
  return {
    events,
    nextToken: res.nextForwardToken ?? nextToken,
    // CloudWatch signals the end by handing back the token you gave it.
    caughtUp: events.length === 0 || res.nextForwardToken === nextToken,
  };
}

/** The most recent events of a stream (the mem-sampler readout). */
export async function readLatest(
  client: CloudWatchLogsClient,
  logGroup: string,
  stream: string,
  limit: number,
): Promise<string[]> {
  const res = await client.send(
    new GetLogEventsCommand({
      logGroupName: logGroup,
      logStreamName: stream,
      startFromHead: false,
      limit,
    }),
  );
  return (res.events ?? []).flatMap((e) => (e.message != null ? [e.message] : []));
}

/** One row of remote-planet.sh's mem sampler: `ts vmrss_kb anon_kb majflt memavail_kb load1`. */
export interface MemSample {
  vmrssGb: number;
  anonGb: number;
  load1: number;
}

export function parseMemSample(messages: string[]): MemSample | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const parts = messages[i].trim().split(/\s+/);
    if (parts.length < 6 || !/^\d+$/.test(parts[0])) continue;
    const [, vmrssKb, anonKb, , , load1] = parts;
    return {
      vmrssGb: Number(vmrssKb) / 1024 / 1024,
      anonGb: Number(anonKb) / 1024 / 1024,
      load1: Number(load1),
    };
  }
  return null;
}

function minOf(a: number | null, b: number | undefined): number | null {
  if (b == null) return a;
  return a === null ? b : Math.min(a, b);
}

function maxOf(a: number | null, b: number | undefined): number | null {
  if (b == null) return a;
  return a === null ? b : Math.max(a, b);
}
