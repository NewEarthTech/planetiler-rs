import { Button } from "@planetiler-rs/ui/button";
import { useState } from "react";
import { isLive, type RunSummary } from "@/lib/runs";

interface RunPickerProps {
  runs: RunSummary[] | null;
  error: string | null;
  /** Selected run id, or null for the demo replay. */
  selected: string | null;
  onSelect: (id: string | null) => void;
}

const MAX_HISTORY_SHOWN = 8;

function ago(ts: number | null): string {
  if (ts === null) return "";
  const mins = Math.round((Date.now() - ts) / 60_000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins} m`;
  const hours = Math.round(mins / 60);
  if (hours < 48) return `${hours} h`;
  return `${Math.round(hours / 24)} d`;
}

/**
 * Source switcher: the demo replay plus the cloud runs the log group knows.
 * Only what deserves attention gets a standing pill — runs emitting now, and
 * whatever is selected. Everything else (cancelled rehearsals, finished
 * benches) collapses behind one history toggle, so a day of smoke tests
 * can't bury the run that is actually in flight.
 */
export function RunPicker({ runs, error, selected, onSelect }: RunPickerProps) {
  const [showHistory, setShowHistory] = useState(false);
  const all = runs ?? [];
  const pinned = all.filter((run) => isLive(run) || run.id === selected);
  const history = all.filter((run) => !pinned.includes(run));

  const pill = (run: RunSummary) => (
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
  );

  return (
    <div className="mb-4 flex flex-wrap items-center gap-2">
      <span className="text-xs text-ink-muted">source</span>
      <Button variant={selected === null ? "solid" : "ghost"} onClick={() => onSelect(null)}>
        demo replay
      </Button>
      {pinned.map(pill)}
      {history.length > 0 && (
        <Button
          variant="ghost"
          className="text-xs text-ink-muted"
          onClick={() => setShowHistory((open) => !open)}
        >
          {showHistory ? "hide history" : `history (${history.length})`}
        </Button>
      )}
      {showHistory && history.slice(0, MAX_HISTORY_SHOWN).map(pill)}
      {runs !== null && runs.length === 0 && (
        <span className="text-xs text-ink-muted">no cloud runs recorded yet</span>
      )}
      {error && <span className="text-xs text-danger">{error}</span>}
    </div>
  );
}
