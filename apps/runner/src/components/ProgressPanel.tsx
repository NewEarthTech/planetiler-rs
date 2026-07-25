import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@planetiler-rs/ui/card";
import { cn, fmtDuration } from "@planetiler-rs/ui/utils";
import type { Counters, RunState } from "@/lib/feed";
import { PHASES, PREPARE_LABEL, type PrepareState, TOTAL_S } from "@/lib/phases";

interface ProgressPanelProps {
  className?: string;
  state: RunState;
  simElapsedS: number;
  phaseDone: Record<string, number>;
  counters: Counters;
  /**
   * The pre-pipeline stage (deps + input download), when the run has one.
   * While it is active the pipeline rows hold at ○ — a downloading box has
   * nothing in pass 1, however long the job log has been flowing. The demo
   * replay omits it: its fixture starts at binary start.
   */
  prepare?: PrepareState | null;
  description?: string;
}

const clamp01 = (v: number) => Math.max(0, Math.min(1, v));
const fmtCount = (n: number | null) => (n === null ? "—" : n.toLocaleString("en-US"));

function Counter({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="tabular text-lg font-semibold tracking-tight">{value}</div>
      <div className="text-xs text-ink-muted">{label}</div>
    </div>
  );
}

export function ProgressPanel({
  className,
  state,
  simElapsedS,
  phaseDone,
  counters,
  prepare = null,
  description = "planet-260713 (93.9 GB) · r7gd.8xlarge · z12 full build — demo replays the 2026-07-22 run at 120×",
}: ProgressPanelProps) {
  const started = state === "running" || state === "done" || state === "cancelled";
  const preparing = prepare?.active ?? false;
  return (
    <Card className={className}>
      <CardHeader>
        <CardTitle>Run progress</CardTitle>
        <CardDescription>{description}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <div className="flex h-2 w-full gap-0.5 overflow-hidden rounded-full">
          {PHASES.map((phase, i) => {
            const startS = i === 0 ? 0 : PHASES[i - 1].endS;
            const fill = started ? clamp01((simElapsedS - startS) / (phase.endS - startS)) : 0;
            return (
              <div
                key={phase.id}
                className="bg-grid"
                style={{ width: `${((phase.endS - startS) / TOTAL_S) * 100}%` }}
              >
                <div
                  className="h-full bg-chart-1 transition-[width] duration-300"
                  style={{ width: `${fill * 100}%` }}
                />
              </div>
            );
          })}
        </div>
        <div className="flex flex-col gap-1.5">
          {prepare && (
            <div className="flex items-center gap-2 text-sm">
              <span
                className={cn(
                  "w-4 text-center",
                  prepare.done && "text-good",
                  prepare.active && "animate-pulse text-chart-1",
                  !prepare.done && !prepare.active && "text-ink-muted",
                )}
                aria-hidden="true"
              >
                {prepare.done ? "✓" : prepare.active ? "●" : "○"}
              </span>
              <span className={prepare.done || prepare.active ? "text-ink" : "text-ink-secondary"}>
                {PREPARE_LABEL}
              </span>
              <span className="tabular ml-auto text-xs text-ink-muted">
                {prepare.elapsedS != null
                  ? fmtDuration(prepare.elapsedS)
                  : prepare.done
                    ? "✓"
                    : "—"}
              </span>
            </div>
          )}
          {PHASES.map((phase, i) => {
            const startS = i === 0 ? 0 : PHASES[i - 1].endS;
            const doneS = phaseDone[phase.id];
            const active =
              state === "running" && !preparing && doneS == null && simElapsedS >= startS;
            return (
              <div key={phase.id} className="flex items-center gap-2 text-sm">
                <span
                  className={cn(
                    "w-4 text-center",
                    doneS != null && "text-good",
                    active && "animate-pulse text-chart-1",
                    doneS == null && !active && "text-ink-muted",
                  )}
                  aria-hidden="true"
                >
                  {doneS != null ? "✓" : active ? "●" : "○"}
                </span>
                <span className={doneS != null || active ? "text-ink" : "text-ink-secondary"}>
                  {phase.label}
                </span>
                <span className="tabular ml-auto text-xs text-ink-muted">
                  {doneS != null
                    ? fmtDuration(doneS)
                    : active
                      ? fmtDuration(simElapsedS - startS)
                      : "—"}
                </span>
              </div>
            );
          })}
        </div>
        <div className="grid grid-cols-2 gap-3 border-t border-grid pt-3 sm:grid-cols-4">
          <Counter label="elapsed" value={started ? fmtDuration(simElapsedS) : "—"} />
          <Counter label="pieces rendered" value={fmtCount(counters.pieces)} />
          <Counter label="entries merged" value={fmtCount(counters.entries)} />
          <Counter label="tiles written" value={fmtCount(counters.tiles)} />
        </div>
      </CardContent>
    </Card>
  );
}
