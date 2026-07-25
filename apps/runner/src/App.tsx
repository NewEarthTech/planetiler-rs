import { Button } from "@planetiler-rs/ui/button";
import { Card, CardContent } from "@planetiler-rs/ui/card";
import { ThemeToggle } from "@planetiler-rs/ui/theme-toggle";
import { type ReactNode, useMemo, useState } from "react";
import { ActionBar } from "@/components/ActionBar";
import { AuthErrorGate, SignInGate } from "@/components/AuthGate";
import { FleetPanel } from "@/components/FleetPanel";
import { LiveRun, LiveRunStream } from "@/components/LiveRun";
import { LogWindow } from "@/components/LogWindow";
import { PlanetGlobe } from "@/components/PlanetGlobe";
import { ProgressPanel } from "@/components/ProgressPanel";
import { RunPicker } from "@/components/RunPicker";
import { RunResults } from "@/components/RunResults";
import { SessionBanner } from "@/components/SessionBanner";
import { StatusChip } from "@/components/StatusChip";
import { type AuthState, useAuth, useSessionExpired } from "@/hooks/useAuth";
import { useDemoRun } from "@/hooks/useDemoRun";
import { useFleet } from "@/hooks/useFleet";
import { useLiveConsole } from "@/hooks/useLiveRuns";
import { demoAffordances } from "@/lib/fleet";
import type { BoxView } from "@/lib/runApi";
import type { RunSummary } from "@/lib/runs";

export default function App() {
  const auth = useAuth();
  if (auth.status === "loading") return null;
  if (auth.status === "error") return <AuthErrorGate message={auth.message} />;
  if (auth.status === "signed-out") return <SignInGate onSignIn={auth.signIn} />;
  return <Console auth={auth} />;
}

function Console({ auth }: { auth: Extract<AuthState, { status: "open" | "signed-in" }> }) {
  const live = useLiveConsole(auth);
  const fleet = useFleet(auth);
  const sessionExpired = useSessionExpired(auth);
  // What am I watching, and what am I acting on: two ids, cross-linked by the
  // handlers below rather than derived from each other, so a historical run
  // whose box is long gone still replays.
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);
  const [selectedBoxId, setSelectedBoxId] = useState<string | null>(null);

  const boxes = fleet?.boxes ?? null;
  const selectedBox = boxes?.find((box) => box.id === selectedBoxId) ?? null;

  const selectRun = (runId: string | null) => {
    setSelectedRunId(runId);
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
    if (runId) setSelectedRunId(runId);
  };

  // A just-dispatched run has no log streams yet, so the 30 s run listing
  // hasn't seen it. Synthesize the summary and let the SSE stream attach now.
  const selectedRun = useMemo<RunSummary | null>(() => {
    if (!selectedRunId) return null;
    return (
      live?.runs?.find((run) => run.id === selectedRunId) ?? {
        id: selectedRunId,
        suffixes: [],
        firstEventAt: null,
        lastEventAt: null,
      }
    );
  }, [live?.runs, selectedRunId]);

  let liveView: ReactNode = null;
  if (selectedRun) {
    if (auth.status === "signed-in" && auth.live) {
      liveView = (
        <LiveRunStream config={auth.live} idToken={auth.session.idToken} run={selectedRun} />
      );
    } else if (live) {
      liveView = <LiveRun client={live.client} logGroup={live.logGroup} run={selectedRun} />;
    }
  }

  // Held as the config object useAuth resolved at boot, so its identity is
  // stable and RunResults doesn't re-list on every render.
  const runsConfig = auth.status === "signed-in" ? auth.runs : null;
  const resultsBucket = runsConfig?.resultsBucket ?? null;

  return (
    <div className="mx-auto max-w-6xl px-4 py-8 sm:px-6">
      <header className="mb-6 flex items-start justify-between gap-4">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">runner</h1>
          <p className="mt-1 max-w-xl text-sm text-ink-secondary">
            Console for planetiler-rs cloud runs: requisition a bench box, dispatch a planet run,
            and watch the box's log live — or replay the first full planet build from its recorded
            log. A/B runs still go out from the laptop (services/cloud-runner).
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

      {fleet && (
        <FleetPanel
          fleet={fleet}
          selectedBoxId={selectedBoxId}
          onSelectBox={selectBox}
          onWatchRun={selectRun}
        />
      )}

      {live && (
        <RunPicker
          runs={live.runs}
          error={live.error}
          selected={selectedRunId}
          onSelect={selectRun}
        />
      )}

      {liveView ??
        (selectedBox ? <IdleBox box={selectedBox} /> : <DemoConsole compact={!!fleet} />)}

      {selectedRun && runsConfig && resultsBucket && auth.status === "signed-in" && (
        <RunResults
          runs={runsConfig}
          bucket={resultsBucket}
          userPoolProvider={auth.userPoolProvider}
          idToken={auth.session.idToken}
          runId={selectedRun.id}
        />
      )}
    </div>
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

function DemoConsole({ compact = false }: { compact?: boolean }) {
  const run = useDemoRun();
  return (
    <>
      {compact ? (
        // The fleet panel owns the real controls; the replay keeps just enough
        // to drive itself, so there is never a simulated bar beside a live one.
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
      ) : (
        <ActionBar
          affordances={demoAffordances(run.state)}
          status={<StatusChip state={run.state} />}
          onRequisition={run.requisition}
          onDispatch={run.dispatch}
          onCancel={run.cancel}
          onDestroy={run.destroy}
        />
      )}

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
