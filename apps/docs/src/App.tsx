import { Card, CardContent } from "@planetiler-rs/ui/card";
import { SERIES_COLOR } from "@planetiler-rs/ui/chart";
import { ThemeToggle } from "@planetiler-rs/ui/theme-toggle";
import { fmtDuration, fmtMemory, fmtUsd } from "@planetiler-rs/ui/utils";
import { BenchChart } from "@/components/BenchChart";
import {
  costUsd,
  EXTRACTS,
  LAST_UPDATED,
  MACHINE_LABEL,
  RUNS,
  type Run,
  TRAJECTORY,
} from "@/data/benchmarks";

function findRun(extract: string, tool: Run["tool"], machine: Run["machine"]) {
  return RUNS.find(
    (run) => run.extract === extract && run.tool === tool && run.machine === machine,
  );
}

/** Cross-scale rows: one per extract, columns per (tool, machine) series. */
function scaleRows(metric: (run: Run) => number | null) {
  return EXTRACTS.map((extract) => ({
    x: extract.label,
    rsLaptop: metric(findRun(extract.id, "planetiler-rs", "laptop-12c") ?? ({} as Run)) ?? null,
    javaLaptop: metric(findRun(extract.id, "planetiler-java", "laptop-12c") ?? ({} as Run)) ?? null,
    rsCloud: (() => {
      const run =
        findRun(extract.id, "planetiler-rs", "r7gd.4xlarge") ??
        findRun(extract.id, "planetiler-rs", "r7g.8xlarge");
      return run ? metric(run) : null;
    })(),
  }));
}

const wall = (run: Run) => run.wallS ?? null;
const rss = (run: Run) => run.peakRssMB ?? null;

function StatTile({ value, label, detail }: { value: string; label: string; detail: string }) {
  return (
    <Card>
      <CardContent className="p-5">
        <div className="text-2xl font-semibold tracking-tight">{value}</div>
        <div className="mt-1 text-sm font-medium text-ink-secondary">{label}</div>
        <div className="mt-0.5 text-xs text-ink-muted">{detail}</div>
      </CardContent>
    </Card>
  );
}

export default function App() {
  const timeData = scaleRows(wall);
  const memoryData = scaleRows(rss);

  const trajectoryRounds = [...new Set(TRAJECTORY.map((point) => point.round))].sort(
    (a, b) => a - b,
  );
  const trajectoryData = trajectoryRounds.map((round) => ({
    x: `round ${round}`,
    gb: TRAJECTORY.find((p) => p.extract === "great-britain" && p.round === round)?.wallS ?? null,
    europe: TRAJECTORY.find((p) => p.extract === "europe" && p.round === round)?.wallS ?? null,
  }));

  const cloudRuns = RUNS.filter((run) => costUsd(run) != null);
  const costData = EXTRACTS.filter((extract) =>
    cloudRuns.some((run) => run.extract === extract.id),
  ).map((extract) => ({
    x: extract.label,
    rs: (() => {
      const run = cloudRuns.find((r) => r.extract === extract.id && r.tool === "planetiler-rs");
      return run ? costUsd(run) : null;
    })(),
  }));

  const gbCloud = findRun("great-britain", "planetiler-rs", "r7gd.4xlarge");
  const monacoRs = findRun("monaco", "planetiler-rs", "laptop-12c")!;
  const monacoJava = findRun("monaco", "planetiler-java", "laptop-12c")!;
  const europeRs = findRun("europe", "planetiler-rs", "laptop-12c")!;
  const europeJava = findRun("europe", "planetiler-java", "laptop-12c")!;

  return (
    <div className="mx-auto max-w-5xl px-4 py-10 sm:px-6">
      <header className="mb-8 flex items-start justify-between gap-4">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">tilebench</h1>
          <p className="mt-1 max-w-xl text-sm text-ink-secondary">
            The running scoreboard for{" "}
            <a
              href="https://github.com/johncarmack1984/planetiler-rs"
              className="font-medium underline decoration-ink-muted underline-offset-2 hover:decoration-ink"
            >
              planetiler-rs
            </a>{" "}
            vs Java{" "}
            <a
              href="https://github.com/onthegomap/planetiler"
              className="font-medium underline decoration-ink-muted underline-offset-2 hover:decoration-ink"
            >
              Planetiler
            </a>
            : same OSM extract in, matching tile output, measured on time, memory, and what the
            build costs on a dedicated cloud box. We win at city and state scale; Planetiler wins at
            country and continent. Both are on the charts.
          </p>
        </div>
        <ThemeToggle storageKey="tilebench-theme" />
      </header>

      <section className="mb-6 grid gap-4 sm:grid-cols-3">
        <StatTile
          value={`${(monacoJava.wallS / monacoRs.wallS).toFixed(1)}×`}
          label="faster at city scale"
          detail={`monaco: ${fmtDuration(monacoRs.wallS)} vs ${fmtDuration(monacoJava.wallS)} (laptop)`}
        />
        <StatTile
          value={`${(europeJava.peakRssMB / europeRs.peakRssMB).toFixed(1)}× less memory`}
          label={`at continent scale, ${(europeRs.wallS / europeJava.wallS).toFixed(1)}× slower`}
          detail={`europe: ${fmtMemory(europeRs.peakRssMB)} vs ${fmtMemory(europeJava.peakRssMB)} RSS, ${fmtDuration(europeRs.wallS)} vs ${fmtDuration(europeJava.wallS)}`}
        />
        <StatTile
          value={gbCloud ? fmtUsd(costUsd(gbCloud)!) : "—"}
          label="to build great-britain"
          detail={
            gbCloud
              ? `${fmtDuration(gbCloud.wallS)} on ${MACHINE_LABEL[gbCloud.machine]}`
              : "cloud run pending"
          }
        />
      </section>

      <main className="grid gap-4 lg:grid-cols-2">
        <BenchChart
          title="Wall time"
          description="Full build, by extract (log scale). Lower is better. The EC2 series is a different machine and a later extract, so read it beside the laptop pair rather than against it."
          data={timeData}
          xKey="x"
          series={[
            { key: "rsLaptop", name: "planetiler-rs", color: SERIES_COLOR["planetiler-rs"] },
            {
              key: "javaLaptop",
              name: "Planetiler (Java)",
              color: SERIES_COLOR["planetiler-java"],
            },
            {
              key: "rsCloud",
              name: "planetiler-rs · EC2",
              color: SERIES_COLOR["planetiler-rs"],
              dashed: true,
            },
          ]}
          format={fmtDuration}
          yTicks={[0.5, 5, 60, 600, 3600]}
          table={{
            columns: ["extract", "planetiler-rs", "Planetiler (Java)", "rs · EC2"],
            rows: timeData.map((row) => [
              row.x as string,
              row.rsLaptop != null ? fmtDuration(row.rsLaptop as number) : null,
              row.javaLaptop != null ? fmtDuration(row.javaLaptop as number) : null,
              row.rsCloud != null ? fmtDuration(row.rsCloud as number) : null,
            ]),
          }}
        />
        <BenchChart
          title="Peak memory"
          description="Peak RSS, by extract (log scale). Lower is better. The EC2 series is a different machine and a later extract, so read it beside the laptop pair rather than against it."
          data={memoryData}
          xKey="x"
          series={[
            { key: "rsLaptop", name: "planetiler-rs", color: SERIES_COLOR["planetiler-rs"] },
            {
              key: "javaLaptop",
              name: "Planetiler (Java)",
              color: SERIES_COLOR["planetiler-java"],
            },
            {
              key: "rsCloud",
              name: "planetiler-rs · EC2",
              color: SERIES_COLOR["planetiler-rs"],
              dashed: true,
            },
          ]}
          format={fmtMemory}
          yTicks={[100, 1024, 10240, 102400]}
          table={{
            columns: ["extract", "planetiler-rs", "Planetiler (Java)", "rs · EC2"],
            rows: memoryData.map((row) => [
              row.x as string,
              row.rsLaptop != null ? fmtMemory(row.rsLaptop as number) : null,
              row.javaLaptop != null ? fmtMemory(row.javaLaptop as number) : null,
              row.rsCloud != null ? fmtMemory(row.rsCloud as number) : null,
            ]),
          }}
        />
        <BenchChart
          title="Optimization trajectory"
          description="planetiler-rs wall time across optimization rounds on the laptop, byte-identical output at every step. Rounds were measured in separate sessions on a shared machine, so the trend is the signal, not the individual deltas."
          data={trajectoryData}
          xKey="x"
          series={[
            { key: "gb", name: "great-britain", color: SERIES_COLOR["great-britain"] },
            { key: "europe", name: "europe", color: SERIES_COLOR.europe },
          ]}
          format={fmtDuration}
          yTicks={[60, 600, 3600]}
          table={{
            columns: ["round", "great-britain", "europe"],
            rows: trajectoryData.map((row) => [
              row.x as string,
              row.gb != null ? fmtDuration(row.gb as number) : null,
              row.europe != null ? fmtDuration(row.europe as number) : null,
            ]),
          }}
        />
        <BenchChart
          title="Cost per build"
          description="On-demand EC2 rate × wall clock, dedicated benchmark boxes (us-east-1). planetiler-rs only so far: Planetiler has not been run on this hardware, so there is no cost comparison to draw yet."
          data={costData}
          xKey="x"
          series={[{ key: "rs", name: "planetiler-rs", color: SERIES_COLOR["planetiler-rs"] }]}
          format={fmtUsd}
          yScale="linear"
          table={{
            columns: ["extract", "planetiler-rs"],
            rows: costData.map((row) => [
              row.x as string,
              row.rs != null ? fmtUsd(row.rs as number) : null,
            ]),
          }}
        />
      </main>

      <footer className="mt-10 border-t border-grid pt-5 text-xs leading-relaxed text-ink-muted">
        <p>
          Method: identical task both sides, OSM extract to osm-qa-style MBTiles (single layer),
          upstream Planetiler pinned at v0.10.2. Cloud numbers are interleaved best-of-N rounds on
          dedicated instances launched per run. Laptop numbers come from the recorded history in{" "}
          <a
            href="https://github.com/johncarmack1984/planetiler-rs/tree/main/bench/results"
            className="underline underline-offset-2"
          >
            bench/results
          </a>
          , measured on a shared development machine where concurrent builds add roughly ±5%. Our
          own output is verified byte-identical across every optimization step. Against Planetiler
          the bar is semantic: tile counts agree within 1% (monaco exact) and shared attributes are
          byte-identical, while 51 of monaco&rsquo;s 102 tiles are semantically identical and the
          rest differ by sub-pixel simplification. Updated {LAST_UPDATED}.
        </p>
      </footer>
    </div>
  );
}
