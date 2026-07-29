import { useEffect, useState } from "react";
import { type LiveConfig, listBackendRuns } from "@/lib/live";
import type { RunSummary, StreamSuffix } from "@/lib/runs";

/** Local runs are minutes long, not hours; the picker should keep up. */
const LIST_POLL_MS = 5_000;

export interface LocalConsole {
  /** Null while the first listing is in flight. */
  runs: RunSummary[] | null;
  error: string | null;
}

/**
 * The run picker for a machine's own runs: the runner-live backend's `/runs`
 * listing, polled. No AWS, no auth — `just ui` starts the backend over
 * `~/.planetiler-rs/runs` and it indexes whatever run directories exist,
 * whether or not they were started while it was up.
 *
 * Returns null when there is no backend configured to ask.
 */
export function useLocalRuns(
  config: LiveConfig | null,
  idToken: string | null,
): LocalConsole | null {
  const [runs, setRuns] = useState<RunSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const url = config?.url ?? null;

  useEffect(() => {
    if (!url) return;
    let cancelled = false;
    const refresh = async () => {
      try {
        const rows = await listBackendRuns({ url }, idToken);
        if (cancelled) return;
        setRuns(
          rows
            .map((row) => ({
              id: row.run,
              suffixes: (row.streams ?? []) as StreamSuffix[],
              firstEventAt: null,
              lastEventAt: row.last_ts || null,
            }))
            .sort((a, b) => (b.lastEventAt ?? 0) - (a.lastEventAt ?? 0)),
        );
        setError(null);
      } catch (err) {
        if (cancelled) return;
        // Nothing listening is the common case, not a crash: say which
        // command starts it rather than surfacing "Failed to fetch".
        setError(
          `${err instanceof Error ? err.message : String(err)} — no run backend at ${url} (start one with \`just ui-server\`)`,
        );
      }
    };
    void refresh();
    const timer = setInterval(() => void refresh(), LIST_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [url, idToken]);

  if (!url) return null;
  return { runs, error };
}
