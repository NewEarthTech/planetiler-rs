import { Button } from "@planetiler-rs/ui/button";
import { Card, CardContent } from "@planetiler-rs/ui/card";
import { ThemeToggle } from "@planetiler-rs/ui/theme-toggle";
import { cn } from "@planetiler-rs/ui/utils";
import { type ReactNode, useMemo, useState } from "react";
import { AuthErrorGate, SignInGate } from "@/components/AuthGate";
import { FleetPanel } from "@/components/FleetPanel";
import { LiveRun, LiveRunStream } from "@/components/LiveRun";
import { LogWindow } from "@/components/LogWindow";
import { PlanetGlobe } from "@/components/PlanetGlobe";
import { ProgressPanel } from "@/components/ProgressPanel";
import { ReplayList } from "@/components/ReplayList";
import { RunResults } from "@/components/RunResults";
import { SessionBanner } from "@/components/SessionBanner";
import { StatusChip } from "@/components/StatusChip";
import { type AuthState, useAuth, useSessionExpired } from "@/hooks/useAuth";
import { useDemoRun } from "@/hooks/useDemoRun";
import { useFleet } from "@/hooks/useFleet";
import { useLiveConsole } from "@/hooks/useLiveRuns";
import type { BoxView } from "@/lib/runApi";
import { isLive, type RunSummary } from "@/lib/runs";

export default function App() {
  const auth = useAuth();
  if (auth.status === "loading") return null;
  if (auth.status === "error") return <AuthErrorGate message={auth.message} />;
  if (auth.status === "signed-out") return <SignInGate onSignIn={auth.signIn} />;
  return <Console auth={auth} />;
}

type Mode = "live" | "replays";

function Console({ auth }: { auth: Extract<AuthState, { status: "open" | "signed-in" }> }) {
  const live = useLiveConsole(auth);
  const fleet = useFleet(auth);
  const sessionExpired = useSessionExpired(auth);
  // Two modes, never on screen together: watching the fleet's runs, and
  // replaying recorded ones. Each keeps its own selection, so flipping tabs
  // doesn't forget what either side was looking at.
  const [mode, setMode] = useState<Mode>("live");
  const [watchRunId, setWatchRunId] = useState<string | null>(null);
  const [replayRunId, setReplayRunId] = useState<string | null>(null);
  const [selectedBoxId, setSelectedBoxId] = useState<string | null>(null);

  const boxes = fleet?.boxes ?? null;
  const selectedBox = boxes?.find((box) => box.id === selectedBoxId) ?? null;

  const watchRun = (runId: string | null) => {
    setWatchRunId(runId);
    setMode("live");
    const owner = runId ? (boxes?.find((box) => box.runId === runId) ?? null) : null;
    setSelectedBoxId((current) => {
      if (owner) return owner.id;
      // Keep an idle box selected: watching an old run shouldn't disarm the
      // box that was just requisitioned.
      const currentBox = boxes?.find((box) => box.id === current) ?? null;
      return currentBox && currentBox.runId === null ? current : null;
    });
  };

  const selectBox = (boxId: string) => {
    setSelectedBoxId(boxId);
    const runId = boxes?.find((box) => box.id === boxId)?.runId ?? null;
    if (runId) setWatchRunId(runId);
  };

  // A just-dispatched run has no log streams yet, so the 30 s run listing
  // hasn't seen it. Synthesize the summary and let the SSE stream attach now.
  const watchedRun = useMemo<RunSummary | null>(() => {
    if (!watchRunId) return null;
    return (
      live?.runs?.find((run) => run.id === watchRunId) ?? {
        id: watchRunId,
        suffixes: [],
        firstEventAt: null,
        lastEventAt: null,
      }
    );
  }, [live?.runs, watchRunId]);

  // Replays are the finished record; anything still emitting lives on the
  // live tab instead.
  const replayRuns = useMemo(() => (live?.runs ?? []).filter((run) => !isLive(run)), [live?.runs]);
  const replayRun = replayRunId
    ? (live?.runs?.find((run) => run.id === replayRunId) ?? null)
    : null;

  const runView = (run: RunSummary): ReactNode => {
    if (auth.status === "signed-in" && auth.live) {
      return <LiveRunStream config={auth.live} idToken={auth.session.idToken} run={run} />;
    }
    if (live) return <LiveRun client={live.client} logGroup={live.logGroup} run={run} />;
    return null;
  };

  // Held as the config object useAuth resolved at boot, so its identity is
  // stable and RunResults doesn't re-list on every render.
  const runsConfig = auth.status === "signed-in" ? auth.runs : null;
  const resultsBucket = runsConfig?.resultsBucket ?? null;

  const results = (runId: string): ReactNode =>
    runsConfig &&
    resultsBucket &&
    auth.status === "signed-in" && (
      <RunResults
        runs={runsConfig}
        bucket={resultsBucket}
        userPoolProvider={auth.userPoolProvider}
        idToken={auth.session.idToken}
        runId={runId}
      />
    );

  return (
    <div className="mx-auto max-w-6xl px-4 py-8 sm:px-6">
      <header className="mb-6 flex items-start justify-between gap-4">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">runner</h1>
          <p className="mt-1 max-w-xl text-sm text-ink-secondary">
            Console for planetiler-rs cloud runs: requisition a bench box, dispatch a planet run,
            and watch the box's log live — or replay a recorded run. A/B runs still go out from the
            laptop (services/cloud-runner).
          </p>
        </div>
        <div className="flex items-center gap-2">
          {auth.status === "signed-in" && (
            <>
              {auth.session.email && (
                <span className="hidden text-xs text-ink-secondary sm:inline">
                  {auth.session.email}
                </span>
              )}
              <Button variant="ghost" onClick={auth.signOut}>
                Sign out
              </Button>
            </>
          )}
          <ThemeToggle storageKey="runner-theme" />
        </div>
      </header>

      {auth.status === "signed-in" && (sessionExpired || fleet?.expired) && (
        <SessionBanner onReauth={auth.reauth} />
      )}

      <div className="mb-4 flex items-center border-b border-grid" role="tablist">
        <ModeTab active={mode === "live"} onClick={() => setMode("live")}>
          live
        </ModeTab>
        <ModeTab active={mode === "replays"} onClick={() => setMode("replays")}>
          replays{replayRuns.length > 0 ? ` (${replayRuns.length})` : ""}
        </ModeTab>
      </div>

      {mode === "live" ? (
        <>
          {fleet && (
            <FleetPanel
              fleet={fleet}
              selectedBoxId={selectedBoxId}
              onSelectBox={selectBox}
              onWatchRun={watchRun}
            />
          )}
          {(watchedRun && runView(watchedRun)) ??
            (selectedBox ? <IdleBox box={selectedBox} /> : <LiveEmpty hasFleet={!!fleet} />)}
          {watchedRun && results(watchedRun.id)}
        </>
      ) : (
        <>
          <ReplayList
            runs={replayRuns}
            error={live?.error ?? null}
            selected={replayRunId}
            onSelect={setReplayRunId}
          />
          {replayRun ? runView(replayRun) : <DemoReplay />}
          {replayRun && results(replayRun.id)}
        </>
      )}
    </div>
  );
}

function ModeTab({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={active}
      onClick={onClick}
      className={cn(
        "-mb-px border-b-2 px-3 py-2 text-sm",
        active
          ? "border-ink font-medium text-ink"
          : "border-transparent text-ink-muted hover:text-ink",
      )}
    >
      {children}
    </button>
  );
}

/** A requisitioned box with nothing dispatched on it yet has no log to show. */
function IdleBox({ box }: { box: BoxView }) {
  return (
    <Card>
      <CardContent className="text-sm text-ink-secondary">
        <span className="font-mono text-xs">{box.id}</span> is {box.state} in {box.region} with no
        job dispatched. Dispatch one to start the stream.
      </CardContent>
    </Card>
  );
}

function LiveEmpty({ hasFleet }: { hasFleet: boolean }) {
  return (
    <Card>
      <CardContent className="text-sm text-ink-secondary">
        {hasFleet
          ? "Nothing being watched. Requisition a box and dispatch a job, or hit watch on a running one — recorded runs are under replays."
          : "This deploy has no dispatch wiring; recorded runs are under replays."}
      </CardContent>
    </Card>
  );
}

/** The checked-in 2026-07-22 planet run, replayed client-side at 120×. */
function DemoReplay() {
  const run = useDemoRun();
  return (
    <>
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="solid" disabled={run.state === "running"} onClick={run.dispatch}>
          Replay planet run
        </Button>
        <Button disabled={run.state !== "running"} onClick={run.cancel}>
          Stop replay
        </Button>
        <div className="ml-auto">
          <StatusChip state={run.state} />
        </div>
      </div>

      <main className="mt-4 grid gap-4 lg:grid-cols-3">
        <ProgressPanel
          className="lg:col-span-2"
          state={run.state}
          simElapsedS={run.simElapsedS}
          phaseDone={run.phaseDone}
          counters={run.counters}
        />
        <PlanetGlobe />
      </main>

      <LogWindow lines={run.lines} className="mt-4" />
    </>
  );
}
