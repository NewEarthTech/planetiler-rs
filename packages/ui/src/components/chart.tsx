import { Collapsible } from "@base-ui-components/react/collapsible";
import { cn } from "@planetiler-rs/ui/utils";

/** Fixed categorical assignment — color follows the entity, everywhere. */
export const SERIES_COLOR = {
  "planetiler-rs": "var(--chart-1)",
  "planetiler-java": "var(--chart-2)",
  branch: "var(--chart-3)",
  "great-britain": "var(--chart-1)",
  europe: "var(--chart-2)",
} as const;

export function LegendChips({
  items,
}: {
  items: { label: string; color: string; dashed?: boolean }[];
}) {
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
      {items.map((item) => (
        <span key={item.label} className="flex items-center gap-1.5 text-xs text-ink-secondary">
          <svg width="16" height="8" aria-hidden="true">
            <line
              x1="0"
              y1="4"
              x2="16"
              y2="4"
              stroke={item.color}
              strokeWidth="2.5"
              strokeDasharray={item.dashed ? "4 3" : undefined}
            />
          </svg>
          {item.label}
        </span>
      ))}
    </div>
  );
}

interface TooltipEntry {
  dataKey?: string | number;
  name?: string | number;
  value?: number | null;
  color?: string;
}

/** shadcn-style tooltip card for Recharts, values formatted by the caller. */
export function ChartTooltip({
  active,
  payload,
  label,
  format,
}: {
  active?: boolean;
  payload?: TooltipEntry[];
  label?: string | number;
  format: (v: number) => string;
}) {
  if (!active || !payload?.length) return null;
  return (
    <div className="rounded-lg border border-ring-hairline bg-surface px-3 py-2 text-xs shadow-md">
      <div className="mb-1 font-medium text-ink">{label}</div>
      <div className="flex flex-col gap-0.5">
        {payload
          .filter((entry) => entry.value != null)
          .map((entry) => (
            <div key={entry.dataKey as string} className="flex items-center gap-1.5">
              <span
                className="inline-block size-2 rounded-[2px]"
                style={{ background: entry.color }}
              />
              <span className="text-ink-secondary">{entry.name}</span>
              <span className="tabular ml-auto pl-3 font-medium text-ink">
                {format(entry.value as number)}
              </span>
            </div>
          ))}
      </div>
    </div>
  );
}

/** Accessible data-table view behind a Base UI collapsible (the relief rule). */
export function DataTable({
  columns,
  rows,
  className,
}: {
  columns: string[];
  rows: (string | null)[][];
  className?: string;
}) {
  return (
    <Collapsible.Root className={cn("mt-3", className)}>
      <Collapsible.Trigger className="group flex items-center gap-1 text-xs font-medium text-ink-muted transition-colors hover:text-ink">
        <svg
          className="size-3 transition-transform group-data-[panel-open]:rotate-90"
          viewBox="0 0 12 12"
          fill="none"
          aria-hidden="true"
        >
          <path d="M4.5 2.5 8 6l-3.5 3.5" stroke="currentColor" strokeWidth="1.5" />
        </svg>
        View data
      </Collapsible.Trigger>
      <Collapsible.Panel>
        <div className="mt-2 overflow-x-auto">
          <table className="w-full border-collapse text-xs">
            <thead>
              <tr className="border-b border-grid text-left text-ink-muted">
                {columns.map((column) => (
                  <th key={column} className="py-1.5 pr-4 font-medium">
                    {column}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((row, i) => (
                <tr key={i} className="border-b border-grid last:border-0">
                  {row.map((cell, j) => (
                    <td
                      key={j}
                      className={cn(
                        "py-1.5 pr-4",
                        j === 0 ? "text-ink-secondary" : "tabular text-ink",
                      )}
                    >
                      {cell ?? "—"}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Collapsible.Panel>
    </Collapsible.Root>
  );
}
