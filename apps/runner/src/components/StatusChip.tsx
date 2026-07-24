import { Badge, type BadgeProps } from "@planetiler-rs/ui/badge";
import { cn } from "@planetiler-rs/ui/utils";
import type { RunState } from "@/lib/feed";

const STATE_CHIP: Record<
  RunState,
  { tone: NonNullable<BadgeProps["tone"]>; label: string; pulse?: boolean }
> = {
  idle: { tone: "neutral", label: "no box" },
  provisioning: { tone: "warn", label: "requisitioning r7gd.8xlarge…", pulse: true },
  ready: { tone: "good", label: "box ready" },
  running: { tone: "info", label: "run in flight", pulse: true },
  done: { tone: "good", label: "run complete" },
  cancelled: { tone: "warn", label: "run cancelled" },
};

export function StatusChip({ state }: { state: RunState }) {
  const chip = STATE_CHIP[state];
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
