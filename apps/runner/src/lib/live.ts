// Client for the runner-live SSE backend (#59): one long-lived fetch per
// run, history first then live from the same response, resumed across
// reconnects with `Last-Event-ID`. Plain fetch + ReadableStream rather than
// EventSource because the Cognito id token rides the Authorization header,
// which EventSource can't set. Deliberately dependency-free — nothing in
// here knows about React, the feed, or AWS.

/** Where the SSE backend lives; published in config.json by the CDK stack. */
export interface LiveConfig {
  /** e.g. https://live.runner.johncarmack.com */
  url: string;
}

export type LiveStreamKind = "job" | "planetiler" | "mem";

/** One `event: line` frame: a log line from one of the run's three streams. */
export interface LiveStreamEvent {
  /** Per-run sequence — the SSE event id, and the resume point. */
  seq: number;
  stream: LiveStreamKind;
  /** Producer-side timestamp, epoch ms — drives the live/stale call. */
  ts: number;
  text: string;
}

export interface StreamRunCallbacks {
  onEvent: (event: LiveStreamEvent) => void;
  /** Fires on every transition: true once attached, false when reconnecting. */
  onConnected: (connected: boolean) => void;
}

const RECONNECT_MIN_MS = 1_000;
const RECONNECT_MAX_MS = 10_000;

/**
 * Stream one run's events until the signal aborts, reconnecting with backoff
 * and resuming from the last delivered seq. Returns after abort; network
 * failures never reject, they reconnect.
 */
export async function streamRun(
  config: LiveConfig,
  runId: string,
  idToken: string,
  signal: AbortSignal,
  callbacks: StreamRunCallbacks,
): Promise<void> {
  let lastSeq = 0;
  let backoffMs = RECONNECT_MIN_MS;
  while (!signal.aborted) {
    try {
      const headers: Record<string, string> = {
        accept: "text/event-stream",
        authorization: `Bearer ${idToken}`,
      };
      if (lastSeq > 0) headers["last-event-id"] = String(lastSeq);
      const res = await fetch(`${config.url}/runs/${encodeURIComponent(runId)}/events`, {
        headers,
        signal,
        cache: "no-store",
      });
      if (!res.ok || !res.body) throw new Error(`event stream: HTTP ${res.status}`);
      callbacks.onConnected(true);
      backoffMs = RECONNECT_MIN_MS;
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        // Frames are delimited by a blank line; keep the trailing partial.
        for (;;) {
          const split = buffer.indexOf("\n\n");
          if (split < 0) break;
          const event = parseSseFrame(buffer.slice(0, split));
          buffer = buffer.slice(split + 2);
          // Skip anything at or before the resume point (the server already
          // filters; this guards the reconnect race).
          if (event && event.seq > lastSeq) {
            lastSeq = event.seq;
            callbacks.onEvent(event);
          }
        }
      }
    } catch {
      // Aborted or network failure — the loop condition decides which.
    }
    if (signal.aborted) return;
    callbacks.onConnected(false);
    await sleep(backoffMs, signal);
    backoffMs = Math.min(backoffMs * 2, RECONNECT_MAX_MS);
  }
}

/**
 * Parse one SSE frame into a line event, or null for anything else
 * (keep-alive comments, unknown event types, malformed payloads).
 */
export function parseSseFrame(frame: string): LiveStreamEvent | null {
  let id = 0;
  let eventType = "message";
  const dataLines: string[] = [];
  for (const raw of frame.split("\n")) {
    if (raw.startsWith(":")) continue;
    const colon = raw.indexOf(":");
    if (colon < 0) continue;
    const field = raw.slice(0, colon);
    const value = raw.startsWith(" ", colon + 1) ? raw.slice(colon + 2) : raw.slice(colon + 1);
    if (field === "id") id = Number(value) || 0;
    else if (field === "event") eventType = value;
    else if (field === "data") dataLines.push(value);
  }
  if (eventType !== "line" || id <= 0 || dataLines.length === 0) return null;
  try {
    const parsed: unknown = JSON.parse(dataLines.join("\n"));
    const { stream, ts, text } = parsed as Partial<LiveStreamEvent>;
    if (
      (stream !== "job" && stream !== "planetiler" && stream !== "mem") ||
      typeof ts !== "number" ||
      typeof text !== "string"
    ) {
      return null;
    }
    return { seq: id, stream, ts, text };
  } catch {
    return null;
  }
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(done, ms);
    signal.addEventListener("abort", done, { once: true });
    function done() {
      clearTimeout(timer);
      signal.removeEventListener("abort", done);
      resolve();
    }
  });
}
