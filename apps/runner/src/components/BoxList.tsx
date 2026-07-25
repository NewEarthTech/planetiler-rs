import { Badge } from "@planetiler-rs/ui/badge";
import { Button } from "@planetiler-rs/ui/button";
import { cn } from "@planetiler-rs/ui/utils";
import { BoxChip } from "@/components/BoxChip";
import { boxAge } from "@/lib/fleet";
import type { BoxView } from "@/lib/runApi";

interface BoxListProps {
  boxes: BoxView[] | null;
  terminating: ReadonlySet<string>;
  selected: string | null;
  onSelect: (id: string) => void;
  onWatchRun: (runId: string) => void;
}

/** The bench boxes currently billing, laptop-dispatched ones included. */
export function BoxList({ boxes, terminating, selected, onSelect, onWatchRun }: BoxListProps) {
  if (boxes === null) {
    return <p className="px-5 py-3 text-xs text-ink-muted">listing boxes…</p>;
  }
  if (boxes.length === 0) {
    return <p className="px-5 py-3 text-xs text-ink-muted">no bench boxes running</p>;
  }
  return (
    <ul className="divide-y divide-grid">
      {boxes.map((box) => {
        const runId = box.runId;
        const ours = box.dispatch === "console";
        const age = boxAge(box.launchTime);
        return (
          <li
            key={box.id}
            className={cn(
              "flex flex-wrap items-center gap-3 px-5 py-2",
              box.id === selected && "bg-page",
            )}
          >
            <button
              type="button"
              onClick={() => onSelect(box.id)}
              className="flex flex-1 flex-wrap items-center gap-3 text-left"
            >
              <BoxChip box={box} terminating={terminating.has(box.id)} />
              <span className="font-mono text-xs">{box.id}</span>
              <span className="text-xs text-ink-muted">
                {box.job ?? "no job"} · {box.instanceType} · {box.region}
              </span>
              {age && <span className="tabular text-xs text-ink-muted">up {age}</span>}
              {box.maxHours != null && (
                <span className="tabular text-xs text-ink-muted">
                  self-terminates ≤{box.maxHours}h
                </span>
              )}
              {box.image && <span className="text-xs text-ink-muted">image {box.image}</span>}
              <Badge tone={ours ? "info" : "neutral"}>{ours ? "console" : "laptop"}</Badge>
            </button>
            {runId && (
              <Button variant="ghost" className="font-mono" onClick={() => onWatchRun(runId)}>
                watch {runId}
              </Button>
            )}
          </li>
        );
      })}
    </ul>
  );
}
