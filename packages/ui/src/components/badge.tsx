import { cn } from "@planetiler-rs/ui/utils";

const TONE_CLASSES = {
  neutral: "border-ring-hairline text-ink-muted",
  info: "border-chart-1/40 text-chart-1",
  good: "border-good/40 text-good",
  warn: "border-chart-3/40 text-chart-3",
  danger: "border-danger/40 text-danger",
} as const;

export interface BadgeProps extends React.ComponentProps<"span"> {
  tone?: keyof typeof TONE_CLASSES;
}

export function Badge({ tone = "neutral", className, ...props }: BadgeProps) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full border bg-surface px-2.5 py-0.5 text-xs font-medium",
        TONE_CLASSES[tone],
        className,
      )}
      {...props}
    />
  );
}
