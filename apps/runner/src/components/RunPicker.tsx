import { Button } from "@planetiler-rs/ui/button";
import { isLive, type RunSummary } from "@/lib/runs";

interface RunPickerProps {
  runs: RunSummary[] | null;
  error: string | null;
  /** Selected run id, or null for the demo replay. */
  selected: string | null;
  onSelect: (id: string | null) => void;
}

const MAX_SHOWN = 8;

function ago(ts: number | null): string {
  if (ts === null) return "";
  const mins = Math.round((Date.now() - ts) / 60_000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins} m`;
  const hours = Math.round(mins / 60);
  if (hours < 48) return `${hours} h`;
  return `${Math.round(hours / 24)} d`;
}

/** Source switcher: the demo replay plus the cloud runs the log group knows. */
export function RunPicker({ runs, error, selected, onSelect }: RunPickerProps) {
  return (
    <div className="mb-4 flex flex-wrap items-center gap-2">
      <span className="text-xs text-ink-muted">source</span>
      <Button variant={selected === null ? "solid" : "ghost"} onClick={() => onSelect(null)}>
        demo replay
      </Button>
      {(runs ?? []).slice(0, MAX_SHOWN).map((run) => (
        <Button
          key={run.id}
          variant={selected === run.id ? "solid" : "ghost"}
          className="font-mono"
          onClick={() => onSelect(run.id)}
        >
          {isLive(run) && (
            <span className="size-1.5 animate-pulse rounded-full bg-current" aria-hidden="true" />
          )}
          {run.id}
          {!isLive(run) && <span className="text-xs opacity-60">{ago(run.lastEventAt)}</span>}
        </Button>
      ))}
      {runs !== null && runs.length === 0 && (
        <span className="text-xs text-ink-muted">no cloud runs recorded yet</span>
      )}
      {error && <span className="text-xs text-danger">{error}</span>}
    </div>
  );
}
