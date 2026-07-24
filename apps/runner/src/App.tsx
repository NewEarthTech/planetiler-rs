import { Button } from "@planetiler-rs/ui/button";
import { ThemeToggle } from "@planetiler-rs/ui/theme-toggle";
import { useState } from "react";
import { ActionBar } from "@/components/ActionBar";
import { AuthErrorGate, SignInGate } from "@/components/AuthGate";
import { LiveRun } from "@/components/LiveRun";
import { LogWindow } from "@/components/LogWindow";
import { PlanetGlobe } from "@/components/PlanetGlobe";
import { ProgressPanel } from "@/components/ProgressPanel";
import { RunPicker } from "@/components/RunPicker";
import { type AuthState, useAuth } from "@/hooks/useAuth";
import { useDemoRun } from "@/hooks/useDemoRun";
import { useLiveConsole } from "@/hooks/useLiveRuns";

export default function App() {
  const auth = useAuth();
  if (auth.status === "loading") return null;
  if (auth.status === "error") return <AuthErrorGate message={auth.message} />;
  if (auth.status === "signed-out") return <SignInGate onSignIn={auth.signIn} />;
  return <Console auth={auth} />;
}

function Console({ auth }: { auth: Extract<AuthState, { status: "open" | "signed-in" }> }) {
  const live = useLiveConsole(auth);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selectedRun = live?.runs?.find((run) => run.id === selectedId) ?? null;
  return (
    <div className="mx-auto max-w-6xl px-4 py-8 sm:px-6">
      <header className="mb-6 flex items-start justify-between gap-4">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">runner</h1>
          <p className="mt-1 max-w-xl text-sm text-ink-secondary">
            Console for planetiler-rs cloud runs: watch a dispatched box's log live, or replay the
            first full planet build from its recorded log. Dispatch still happens from the laptop
            (services/cloud-runner).
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

      {live && (
        <RunPicker
          runs={live.runs}
          error={live.error}
          selected={selectedRun?.id ?? null}
          onSelect={setSelectedId}
        />
      )}

      {live && selectedRun ? (
        <LiveRun client={live.client} logGroup={live.logGroup} run={selectedRun} />
      ) : (
        <DemoConsole />
      )}
    </div>
  );
}

function DemoConsole() {
  const run = useDemoRun();
  return (
    <>
      <ActionBar
        state={run.state}
        onRequisition={run.requisition}
        onDispatch={run.dispatch}
        onCancel={run.cancel}
        onDestroy={run.destroy}
      />

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
