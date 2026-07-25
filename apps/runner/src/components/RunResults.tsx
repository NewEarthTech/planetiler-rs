import { Button } from "@planetiler-rs/ui/button";
import { Card, CardDescription, CardHeader, CardTitle } from "@planetiler-rs/ui/card";
import { useCallback, useEffect, useRef, useState } from "react";
import type { RunsConfig } from "@/lib/auth";
import type { ResultObject } from "@/lib/results";

interface RunResultsProps {
  runs: RunsConfig;
  bucket: string;
  userPoolProvider: string;
  idToken: string;
  runId: string;
}

/** What the box shipped to S3 for this run: the report inline, the rest as links. */
export function RunResults({ runs, bucket, userPoolProvider, idToken, runId }: RunResultsProps) {
  const [results, setResults] = useState<ResultObject[] | null>(null);
  const [report, setReport] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Switching runs or hitting refresh supersedes whatever is in flight.
  const request = useRef(0);

  const load = useCallback(async () => {
    const mine = ++request.current;
    setResults(null);
    setReport(null);
    setError(null);
    try {
      // Dynamic: the S3 client stays out of the bundle until a run is picked.
      const { fetchText, findReport, listResults, makeResultsClient } = await import(
        "@/lib/results"
      );
      const client = makeResultsClient(runs, userPoolProvider, idToken);
      const listed = await listResults(client, bucket, runId);
      if (request.current !== mine) return;
      setResults(listed);
      const found = findReport(listed);
      if (!found) return;
      const text = await fetchText(client, bucket, found.key);
      if (request.current === mine) setReport(text);
    } catch (err) {
      if (request.current === mine) setError(err instanceof Error ? err.message : String(err));
    }
  }, [runs, bucket, userPoolProvider, idToken, runId]);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <Card className="mt-4">
      <CardHeader className="flex-row items-center justify-between gap-3">
        <div className="flex flex-col gap-1">
          <CardTitle>Results</CardTitle>
          <CardDescription>
            s3://{bucket}/{runId}/ — shipped when the box finishes or fails, and expired after 90
            days. bench/results/ stays the record.
          </CardDescription>
        </div>
        <Button variant="ghost" onClick={() => void load()}>
          refresh
        </Button>
      </CardHeader>

      <div className="px-5 pb-5">
        {error && <p className="text-xs text-danger">{error}</p>}
        {!error && results === null && (
          <p className="text-xs text-ink-muted">reading the bucket…</p>
        )}
        {!error && results?.length === 0 && (
          <p className="text-xs text-ink-muted">
            no results yet — they ship when the box finishes or fails.
          </p>
        )}

        {report && (
          <pre className="mb-3 max-h-80 overflow-auto rounded-lg border border-ring-hairline bg-page p-3 text-xs">
            {report}
          </pre>
        )}

        {results && results.length > 0 && (
          <ul className="divide-y divide-grid">
            {results.map((result) => (
              <li key={result.key} className="flex flex-wrap items-center gap-3 py-1.5">
                <span className="flex-1 font-mono text-xs">{result.name}</span>
                <span className="tabular text-xs text-ink-muted">{fmtBytes(result.size)}</span>
                <a
                  href={result.url}
                  download
                  className="text-xs text-ink-secondary underline hover:text-ink"
                >
                  download
                </a>
              </li>
            ))}
          </ul>
        )}
      </div>
    </Card>
  );
}

function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${Math.round(kb)} KB`;
  const mb = kb / 1024;
  return mb < 10 ? `${mb.toFixed(1)} MB` : `${Math.round(mb)} MB`;
}
