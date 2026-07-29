import { Badge } from "@planetiler-rs/ui/badge";
import { cn } from "@planetiler-rs/ui/utils";
import { isLive, type RunSummary } from "@/lib/runs";

interface LocalRunListProps {
  /** Null while the first listing is in flight. */
  runs: RunSummary[] | null;
  error: string | null;
  selected: string | null;
  onSelect: (id: string) => void;
}

function when(ts: number | null): string {
  if (ts === null) return "";
  const iso = new Date(ts).toISOString();
  return `${iso.slice(0, 10)} ${iso.slice(11, 16)}Z`;
}

/**
 * The picker for runs on this machine — the local stand-in for the fleet
 * panel, which has boxes to list and this has directories. Newest first, in
 * flight ones badged: a run started from another terminal shows up here within
 * a poll, and selecting it attaches the stream from its first line.
 */
export function LocalRunList({ runs, error, selected, onSelect }: LocalRunListProps) {
  return (
    <div className="mb-4 flex flex-col gap-2">
      <div className="flex flex-wrap items-baseline gap-2">
        <span className="text-xs text-ink-muted">runs on this machine</span>
        <span className="font-mono text-xs text-ink-muted">~/.planetiler-rs/runs</span>
        {error && <span className="text-xs text-danger">{error}</span>}
      </div>
      <div className="max-h-72 divide-y divide-grid overflow-y-auto rounded-md border border-grid">
        {(runs ?? []).map((run) => (
          <button
            key={run.id}
            type="button"
            onClick={() => onSelect(run.id)}
            className={cn(
              "flex w-full items-baseline justify-between gap-3 px-3 py-1.5 text-left text-sm hover:bg-page",
              selected === run.id && "bg-page",
            )}
          >
            <span className="flex items-baseline gap-2">
              <span
                className={cn(
                  "font-mono text-xs",
                  selected === run.id ? "text-ink" : "text-ink-secondary",
                )}
              >
                {run.id}
              </span>
              {isLive(run) && (
                <Badge tone="info">
                  <span className="size-1.5 animate-pulse rounded-full bg-current" aria-hidden />
                  live
                </Badge>
              )}
            </span>
            <span className="shrink-0 text-xs text-ink-muted">{when(run.lastEventAt)}</span>
          </button>
        ))}
        {runs?.length === 0 && (
          <p className="px-3 py-2 text-xs text-ink-muted">
            No runs yet — start one with <span className="font-mono">just run monaco</span>. This
            list follows the directory, so a run started before the console was up shows up too.
          </p>
        )}
        {runs === null && !error && <p className="px-3 py-2 text-xs text-ink-muted">loading…</p>}
      </div>
    </div>
  );
}
