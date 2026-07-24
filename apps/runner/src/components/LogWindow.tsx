import { Card } from "@planetiler-rs/ui/card";
import { cn } from "@planetiler-rs/ui/utils";
import { useEffect, useRef } from "react";
import type { LogLine } from "@/lib/log";

export function LogWindow({
  lines,
  className,
  title = "planet-rs.stderr",
  emptyText = "No run in flight — requisition a box and dispatch the job to replay the planet demo.",
}: {
  lines: LogLine[];
  className?: string;
  title?: string;
  emptyText?: string;
}) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const stuckRef = useRef(true);

  // Stick to the bottom while the user hasn't scrolled up.
  useEffect(() => {
    if (lines.length === 0) return;
    const el = scrollRef.current;
    if (el && stuckRef.current) el.scrollTop = el.scrollHeight;
  }, [lines.length]);

  return (
    <Card className={cn("overflow-hidden", className)}>
      <div className="flex items-center justify-between border-b border-grid px-4 py-2">
        <span className="font-mono text-xs text-ink-secondary">{title}</span>
        <span className="tabular text-xs text-ink-muted">
          {lines.length > 0 ? `${lines.length} lines` : "idle"}
        </span>
      </div>
      <div
        ref={scrollRef}
        onScroll={() => {
          const el = scrollRef.current;
          if (el) stuckRef.current = el.scrollTop + el.clientHeight >= el.scrollHeight - 8;
        }}
        className="h-80 overflow-auto px-4 py-2 font-mono text-xs leading-relaxed"
      >
        {lines.length === 0 ? (
          <div className="flex h-full items-center justify-center text-ink-muted">{emptyText}</div>
        ) : (
          lines.map((line, i) => (
            <div
              key={i}
              className={cn(
                "flex gap-2 whitespace-nowrap",
                line.level === "WARN" && "text-chart-3",
              )}
            >
              <span className="shrink-0 text-ink-muted">{line.time}</span>
              <span
                className={cn(
                  "w-10 shrink-0",
                  line.level === "WARN" ? "font-semibold" : "text-ink-muted",
                )}
              >
                {line.level}
              </span>
              <span className="shrink-0 text-ink-muted">{line.target}</span>
              <span className={cn(line.level === "INFO" && "text-ink-secondary")}>{line.tail}</span>
            </div>
          ))
        )}
      </div>
    </Card>
  );
}
