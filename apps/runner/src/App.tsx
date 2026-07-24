import { ThemeToggle } from "@planetiler-rs/ui/theme-toggle";
import { ActionBar } from "@/components/ActionBar";
import { LogWindow } from "@/components/LogWindow";
import { PlanetGlobe } from "@/components/PlanetGlobe";
import { ProgressPanel } from "@/components/ProgressPanel";
import { useDemoRun } from "@/hooks/useDemoRun";

export default function App() {
  const run = useDemoRun();
  return (
    <div className="mx-auto max-w-6xl px-4 py-8 sm:px-6">
      <header className="mb-6 flex items-start justify-between gap-4">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">runner</h1>
          <p className="mt-1 max-w-xl text-sm text-ink-secondary">
            Console for planetiler-rs cloud runs: requisition a box, dispatch a job, watch the log,
            destroy the box. Nothing is wired to AWS yet — Dispatch replays the first full planet
            build from its recorded log.
          </p>
        </div>
        <ThemeToggle storageKey="runner-theme" />
      </header>

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
    </div>
  );
}
