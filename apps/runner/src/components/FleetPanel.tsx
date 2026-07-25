import { Button } from "@planetiler-rs/ui/button";
import { Card, CardDescription, CardHeader, CardTitle } from "@planetiler-rs/ui/card";
import { fmtUsd } from "@planetiler-rs/ui/utils";
import { useState } from "react";
import { ActionBar } from "@/components/ActionBar";
import { BoxChip } from "@/components/BoxChip";
import { BoxList } from "@/components/BoxList";
import { Field } from "@/components/Field";
import type { Fleet } from "@/hooks/useFleet";
import { boxAffordances, DISPATCHABLE_JOBS, type DispatchableJob } from "@/lib/fleet";
import { checkIdent, checkMaxHours, MAX_HOURS_MAX, MAX_HOURS_MIN } from "@/lib/runApi";

const DEFAULT_IMAGE_TAG = "main";

/**
 * run-api's own per-job defaults (jobs.rs), restated so the form shows them.
 * The hourly figures make the ceiling legible before clicking, not a bill:
 * planet is backed out of the ~$5 recorded for the 2 h 55 m run in
 * bench/results/planet-preflight.md; smoke is the r7gd.large on-demand rate.
 */
const JOB_PRESETS: Record<
  DispatchableJob,
  { instanceType: string; maxHours: string; hourlyUsd: number; blurb: string }
> = {
  planet: {
    instanceType: "r7gd.8xlarge",
    maxHours: "14",
    hourlyUsd: 1.7,
    blurb: "the full 94 GB planet build vs upstream",
  },
  "planet-smoke": {
    instanceType: "r7gd.large",
    maxHours: "1",
    hourlyUsd: 0.15,
    blurb: "the same pipeline on monaco — proves every leg in minutes",
  },
};

interface FleetPanelProps {
  fleet: Fleet;
  selectedBoxId: string | null;
  onSelectBox: (id: string) => void;
  onWatchRun: (runId: string) => void;
}

/**
 * The bench fleet and the controls that change it. This panel owns the only
 * ActionBar whenever the console has dispatch wiring — the demo replay drops
 * to its compact controls so there is never a simulated bar next to a real one.
 */
export function FleetPanel({ fleet, selectedBoxId, onSelectBox, onWatchRun }: FleetPanelProps) {
  const [showOptions, setShowOptions] = useState(false);
  const [job, setJob] = useState<DispatchableJob>("planet");
  const [imageTag, setImageTag] = useState(DEFAULT_IMAGE_TAG);
  const [maxHours, setMaxHours] = useState(JOB_PRESETS.planet.maxHours);
  const preset = JOB_PRESETS[job];

  // Picking a job re-arms its own failsafe default: a 14 h ceiling carried
  // from planet onto a smoke box would be exactly the stale-form lie the
  // shakedown flagged, in the other direction.
  const selectJob = (next: DispatchableJob) => {
    setJob(next);
    setMaxHours(JOB_PRESETS[next].maxHours);
  };

  const selectedBox = fleet.boxes?.find((box) => box.id === selectedBoxId) ?? null;
  const terminating = selectedBox ? fleet.terminating.has(selectedBox.id) : false;
  const affordances = boxAffordances(selectedBox, terminating);

  const imageTagError = checkIdent(imageTag, "imageTag");
  const hours = Number(maxHours);
  const maxHoursError = checkMaxHours(hours);
  const optionsValid = !imageTagError && !maxHoursError;

  const requisition = async () => {
    if (!optionsValid) {
      setShowOptions(true);
      return;
    }
    const id = await fleet.requisition({ job, imageTag, maxHours: hours });
    if (id) onSelectBox(id);
  };

  const dispatch = async () => {
    if (!selectedBox) return;
    const runId = await fleet.dispatch(selectedBox.id);
    if (runId) onWatchRun(runId);
  };

  return (
    <Card className="mb-4">
      <CardHeader className="flex-row items-center justify-between gap-3">
        <div className="flex flex-col gap-1">
          <CardTitle>Fleet</CardTitle>
          <CardDescription>
            planet runs and their smoke rehearsal; A/B stays on the laptop path until its dispatch
            wiring lands.
          </CardDescription>
        </div>
        <div className="flex items-center gap-2">
          {fleet.error && <span className="text-xs text-danger">{fleet.error}</span>}
          <Button variant="ghost" onClick={fleet.refresh}>
            refresh
          </Button>
        </div>
      </CardHeader>

      <BoxList
        boxes={fleet.boxes}
        terminating={fleet.terminating}
        selected={selectedBoxId}
        onSelect={onSelectBox}
        onWatchRun={onWatchRun}
      />

      <div className="border-grid border-t p-5">
        <ActionBar
          key={selectedBoxId ?? "none"}
          affordances={affordances}
          pending={fleet.pending}
          error={fleet.actionError}
          onDismissError={fleet.dismissError}
          status={selectedBox ? <BoxChip box={selectedBox} terminating={terminating} /> : undefined}
          confirmDestroy
          onRequisition={() => void requisition()}
          onDispatch={() => void dispatch()}
          onCancel={() => selectedBox && void fleet.cancel(selectedBox.id)}
          onDestroy={() => selectedBox && void fleet.destroy(selectedBox.id)}
        >
          <div className="flex flex-col gap-2">
            <div className="flex flex-wrap items-center gap-2">
              {DISPATCHABLE_JOBS.map((candidate) => (
                <Button
                  key={candidate}
                  variant={candidate === job ? "solid" : "ghost"}
                  className="h-7 px-2 font-mono text-xs"
                  onClick={() => selectJob(candidate)}
                >
                  {candidate}
                </Button>
              ))}
              <span className="text-xs text-ink-muted">{preset.blurb}</span>
            </div>
            <button
              type="button"
              className="w-fit text-xs text-ink-muted underline"
              onClick={() => setShowOptions((open) => !open)}
            >
              {showOptions ? "hide options" : "options"} · {preset.instanceType} · image {imageTag}{" "}
              · terminates itself after {maxHours} h
            </button>
            {showOptions && (
              <div className="grid gap-3 sm:grid-cols-2">
                <Field
                  label="image tag"
                  value={imageTag}
                  onChange={setImageTag}
                  error={imageTagError}
                  hint="the runner image CI pushed: main, or a commit sha"
                />
                <Field
                  label="max hours"
                  value={maxHours}
                  onChange={setMaxHours}
                  inputMode="numeric"
                  error={maxHoursError}
                  hint={
                    maxHoursError
                      ? undefined
                      : `${MAX_HOURS_MIN}-${MAX_HOURS_MAX}; the box self-terminates at the ceiling, about ${fmtUsd(
                          hours * preset.hourlyUsd,
                        )} of ${preset.instanceType}`
                  }
                />
              </div>
            )}
          </div>
        </ActionBar>
      </div>
    </Card>
  );
}
