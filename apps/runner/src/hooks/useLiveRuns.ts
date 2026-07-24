import type { CloudWatchLogsClient } from "@aws-sdk/client-cloudwatch-logs";
import { useEffect, useMemo, useState } from "react";
import type { AuthState } from "@/hooks/useAuth";
import { listRuns, makeRunsClient, type RunSummary } from "@/lib/runs";

const LIST_POLL_MS = 30_000;

export interface LiveConsole {
  client: CloudWatchLogsClient;
  logGroup: string;
  /** Null while the first listing is in flight. */
  runs: RunSummary[] | null;
  error: string | null;
}

/**
 * The live side of the console: a CloudWatch Logs client on identity-pool
 * credentials plus a periodically refreshed run listing. Null when the
 * console has no live wiring — not signed in (dev), or a deploy without the
 * run-logs infra.
 */
export function useLiveConsole(auth: AuthState): LiveConsole | null {
  const runsCfg = auth.status === "signed-in" ? auth.runs : null;
  const provider = auth.status === "signed-in" ? auth.userPoolProvider : "";
  const idToken = auth.status === "signed-in" ? auth.session.idToken : "";

  const client = useMemo(
    () => (runsCfg ? makeRunsClient(runsCfg, provider, idToken) : null),
    [runsCfg, provider, idToken],
  );

  const [runs, setRuns] = useState<RunSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!client || !runsCfg) return;
    let cancelled = false;
    const refresh = async () => {
      try {
        const next = await listRuns(client, runsCfg.logGroup);
        if (!cancelled) {
          setRuns(next);
          setError(null);
        }
      } catch (err) {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      }
    };
    void refresh();
    const timer = setInterval(() => void refresh(), LIST_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [client, runsCfg]);

  if (!client || !runsCfg) return null;
  return { client, logGroup: runsCfg.logGroup, runs, error };
}
