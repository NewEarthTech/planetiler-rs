import { Badge } from "@planetiler-rs/ui/badge";
import { cn } from "@planetiler-rs/ui/utils";
import { boxChip } from "@/lib/fleet";
import type { BoxView } from "@/lib/runApi";

/** The EC2 lifecycle of one box. Distinct from StatusChip, which reports a run. */
export function BoxChip({ box, terminating }: { box: BoxView; terminating: boolean }) {
  const chip = boxChip(box, terminating);
  return (
    <Badge tone={chip.tone}>
      <span
        className={cn("size-1.5 rounded-full bg-current", chip.pulse && "animate-pulse")}
        aria-hidden="true"
      />
      {chip.label}
    </Badge>
  );
}
