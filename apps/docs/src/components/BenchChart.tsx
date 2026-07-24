import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@planetiler-rs/ui/card";
import { ChartTooltip, DataTable, LegendChips } from "@planetiler-rs/ui/chart";
import {
  CartesianGrid,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";

export interface SeriesSpec {
  key: string;
  name: string;
  color: string;
  dashed?: boolean;
}

export interface BenchChartProps {
  title: string;
  description: string;
  data: Record<string, string | number | null>[];
  xKey: string;
  series: SeriesSpec[];
  format: (v: number) => string;
  yScale?: "log" | "linear";
  yTicks?: number[];
  table: { columns: string[]; rows: (string | null)[][] };
}

/** One end-of-line direct label per series (relief for sub-3:1 hues). */
function endLabel(data: BenchChartProps["data"], seriesKey: string, name: string) {
  const last = data.reduce((found, row, i) => (row[seriesKey] != null ? i : found), -1);
  return (props: { x?: number | string; y?: number | string; index?: number }) => {
    if (props.index !== last || props.x == null || props.y == null) return <g />;
    return (
      <text
        x={Number(props.x) + 9}
        y={Number(props.y) + 3.5}
        fontSize={11}
        fill="var(--ink-secondary)"
      >
        {name}
      </text>
    );
  };
}

export function BenchChart({
  title,
  description,
  data,
  xKey,
  series: allSeries,
  format,
  yScale = "log",
  yTicks,
  table,
}: BenchChartProps) {
  // A series with no data yet doesn't get a line or a legend chip promising one.
  const series = allSeries.filter((s) => data.some((row) => row[s.key] != null));
  return (
    <Card>
      <CardHeader>
        <div className="flex items-start justify-between gap-4">
          <div>
            <CardTitle>{title}</CardTitle>
            <CardDescription className="mt-1">{description}</CardDescription>
          </div>
        </div>
        <div className="mt-2">
          <LegendChips
            items={series.map((s) => ({ label: s.name, color: s.color, dashed: s.dashed }))}
          />
        </div>
      </CardHeader>
      <CardContent>
        <div className="h-64">
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={data} margin={{ top: 8, right: 110, bottom: 4, left: 4 }}>
              <CartesianGrid stroke="var(--grid)" strokeWidth={1} vertical={false} />
              <XAxis
                dataKey={xKey}
                tickLine={false}
                axisLine={{ stroke: "var(--baseline)" }}
                tick={{ fill: "var(--ink-muted)", fontSize: 11 }}
                tickMargin={8}
              />
              <YAxis
                scale={yScale}
                domain={yScale === "log" ? ["auto", "auto"] : [0, "auto"]}
                ticks={yTicks}
                tickLine={false}
                axisLine={false}
                tick={{ fill: "var(--ink-muted)", fontSize: 11 }}
                tickFormatter={format}
                width={58}
              />
              <Tooltip
                cursor={{ stroke: "var(--baseline)", strokeDasharray: "3 3" }}
                content={<ChartTooltip format={format} />}
              />
              {series.map((s) => (
                <Line
                  key={s.key}
                  dataKey={s.key}
                  name={s.name}
                  type="monotone"
                  stroke={s.color}
                  strokeWidth={2}
                  strokeDasharray={s.dashed ? "5 4" : undefined}
                  dot={{ r: 3, fill: s.color, strokeWidth: 0 }}
                  activeDot={{ r: 5, stroke: "var(--surface)", strokeWidth: 2 }}
                  connectNulls
                  isAnimationActive={false}
                  label={endLabel(data, s.key, s.name)}
                />
              ))}
            </LineChart>
          </ResponsiveContainer>
        </div>
        <DataTable columns={table.columns} rows={table.rows} />
      </CardContent>
    </Card>
  );
}
