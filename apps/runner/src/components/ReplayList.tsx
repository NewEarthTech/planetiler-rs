import { Button } from "@planetiler-rs/ui/button";
import { cn } from "@planetiler-rs/ui/utils";
import { useState } from "react";
import { type RunSummary, runInput } from "@/lib/runs";

interface ReplayListProps {
  /** Finished runs only — live ones belong to the live tab. Newest first. */
  runs: RunSummary[];
  error: string | null;
  /** Selected run id, or null for the demo replay. */
  selected: string | null;
  onSelect: (id: string | null) => void;
}

/** The inputs the corpus actually runs; "all" is the resting state. */
const INPUT_FILTERS = ["all", "monaco", "rhode island", "great britain", "europe", "planet"];

function when(ts: number | null): string {
  if (ts === null) return "";
  const d = new Date(ts);
  return `${d.toISOString().slice(0, 10)} ${d.toISOString().slice(11, 16)}Z`;
}

/**
 * Recorded runs as a vertical, date-sorted, input-filterable list — a
 * scrolling archive, not a pill row: a day of rehearsals reads as history,
 * not as eight competing buttons. The demo replay (the checked-in 2026-07-22
 * fixture) is pinned on top as the null selection.
 */
export function ReplayList({ runs, error, selected, onSelect }: ReplayListProps) {
  const [filter, setFilter] = useState("all");
  const shown = filter === "all" ? runs : runs.filter((run) => runInput(run.id) === filter);

  const row = (id: string | null, label: string, right: string, active: boolean) => (
    <button
      key={id ?? "demo"}
      type="button"
      onClick={() => onSelect(id)}
      className={cn(
        "flex w-full items-baseline justify-between gap-3 px-3 py-1.5 text-left text-sm hover:bg-page",
        active && "bg-page",
      )}
    >
      <span className={cn("font-mono text-xs", active ? "text-ink" : "text-ink-secondary")}>
        {label}
      </span>
      <span className="shrink-0 text-xs text-ink-muted">{right}</span>
    </button>
  );

  return (
    <div className="mb-4 flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-1">
        <span className="mr-1 text-xs text-ink-muted">input</span>
        {INPUT_FILTERS.map((input) => (
          <Button
            key={input}
            variant={filter === input ? "solid" : "ghost"}
            className="h-6 px-2 text-xs"
            onClick={() => setFilter(input)}
          >
            {input}
          </Button>
        ))}
        {error && <span className="ml-2 text-xs text-danger">{error}</span>}
      </div>
      <div className="max-h-72 divide-y divide-grid overflow-y-auto rounded-md border border-grid">
        {row(null, "demo replay — recorded 2026-07-22 planet run at 120×", "", selected === null)}
        {shown.map((run) => row(run.id, run.id, when(run.lastEventAt), selected === run.id))}
        {shown.length === 0 && (
          <p className="px-3 py-2 text-xs text-ink-muted">
            {runs.length === 0 ? "no recorded runs yet" : `no ${filter} runs recorded`}
          </p>
        )}
      </div>
    </div>
  );
}
