import type { CloudWatchLogsClient } from "@aws-sdk/client-cloudwatch-logs";
import { Badge, type BadgeProps } from "@planetiler-rs/ui/badge";
import { cn } from "@planetiler-rs/ui/utils";
import { LogWindow } from "@/components/LogWindow";
import { PlanetGlobe } from "@/components/PlanetGlobe";
import { ProgressPanel } from "@/components/ProgressPanel";
import { type LiveRun as LiveRunData, type LiveStatus, useLiveRun } from "@/hooks/useLiveRun";
import { useLiveRunSse } from "@/hooks/useLiveRunSse";
import type { RunState } from "@/lib/feed";
import type { LiveConfig } from "@/lib/live";
import type { RunSummary } from "@/lib/runs";

const STATUS_CHIP: Record<
  LiveStatus,
  { tone: NonNullable<BadgeProps["tone"]>; label: string; pulse?: boolean }
> = {
  connecting: { tone: "neutral", label: "connecting…", pulse: true },
  live: { tone: "info", label: "run in flight", pulse: true },
  done: { tone: "good", label: "run complete" },
  stale: { tone: "warn", label: "no recent events" },
};

/** How the live tail maps onto the progress panel's demo state machine. */
const PANEL_STATE: Record<LiveStatus, RunState> = {
  connecting: "ready",
  live: "running",
  done: "done",
  stale: "cancelled",
};

interface LiveRunProps {
  client: CloudWatchLogsClient;
  logGroup: string;
  run: RunSummary;
}

/** A cloud run, tailed by polling CloudWatch Logs — the fallback path when the deploy has no SSE backend. */
export function LiveRun({ client, logGroup, run }: LiveRunProps) {
  const live = useLiveRun(client, logGroup, run);
  return <LiveRunView run={run} live={live} />;
}

interface LiveRunStreamProps {
  config: LiveConfig;
  idToken: string;
  run: RunSummary;
}

/** A cloud run, streamed line-by-line over SSE from the runner-live backend. */
export function LiveRunStream({ config, idToken, run }: LiveRunStreamProps) {
  const live = useLiveRunSse(config, idToken, run);
  return <LiveRunView run={run} live={live} />;
}

function LiveRunView({ run, live }: { run: RunSummary; live: LiveRunData }) {
  const chip = STATUS_CHIP[live.status];
  return (
    <>
      <div className="flex flex-wrap items-center gap-3">
        <span className="font-mono text-sm">{run.id}</span>
        <Badge tone={chip.tone}>
          <span
            className={cn("size-1.5 rounded-full bg-current", chip.pulse && "animate-pulse")}
            aria-hidden="true"
          />
          {chip.label}
        </Badge>
        {live.mem && (
          <span className="tabular text-xs text-ink-muted">
            RSS {live.mem.vmrssGb.toFixed(1)} GB · anon {live.mem.anonGb.toFixed(1)} GB · load{" "}
            {live.mem.load1.toFixed(1)}
          </span>
        )}
        {live.error && <span className="text-xs text-danger">{live.error}</span>}
      </div>

      <main className="mt-4 grid gap-4 lg:grid-cols-3">
        <ProgressPanel
          className="lg:col-span-2"
          state={PANEL_STATE[live.status]}
          simElapsedS={live.feed.simElapsedS}
          phaseDone={live.feed.phaseDone}
          counters={live.feed.counters}
          description="live from the box's log drain — phase widths referenced to the 2026-07-22 planet run"
        />
        <PlanetGlobe />
      </main>

      <LogWindow
        lines={live.feed.lines}
        className="mt-4"
        title={live.streamName}
        emptyText="Waiting for the first log events from the box…"
      />
    </>
  );
}
