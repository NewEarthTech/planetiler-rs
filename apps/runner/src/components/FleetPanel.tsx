import { Button } from "@planetiler-rs/ui/button";
import { Card, CardDescription, CardHeader, CardTitle } from "@planetiler-rs/ui/card";
import { fmtUsd } from "@planetiler-rs/ui/utils";
import { useState } from "react";
import { ActionBar } from "@/components/ActionBar";
import { BoxChip } from "@/components/BoxChip";
import { BoxList } from "@/components/BoxList";
import { Field } from "@/components/Field";
import type { Fleet } from "@/hooks/useFleet";
import { boxAffordances, DISPATCHABLE_JOB } from "@/lib/fleet";
import { checkIdent, checkMaxHours, MAX_HOURS_MAX, MAX_HOURS_MIN } from "@/lib/runApi";

/** run-api's own defaults (jobs.rs:42-56), restated so the form shows them. */
const DEFAULT_IMAGE_TAG = "main";
const DEFAULT_MAX_HOURS = "14";
const DEFAULT_INSTANCE_TYPE = "r7gd.8xlarge";
/**
 * Rough hourly rate for the default box, backed out of the ~$5 recorded for
 * the 2 h 55 m planet run in bench/results/planet-preflight.md. It exists to
 * make the ceiling legible before clicking, not to be a billing figure.
 */
const PLANET_HOURLY_USD = 1.7;

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
  const [imageTag, setImageTag] = useState(DEFAULT_IMAGE_TAG);
  const [maxHours, setMaxHours] = useState(DEFAULT_MAX_HOURS);

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
    const id = await fleet.requisition({ job: DISPATCHABLE_JOB, imageTag, maxHours: hours });
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
            {DISPATCHABLE_JOB} runs only until image.yml builds branch images; A/B stays on the
            laptop path.
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
            <button
              type="button"
              className="w-fit text-xs text-ink-muted underline"
              onClick={() => setShowOptions((open) => !open)}
            >
              {showOptions ? "hide options" : "options"} · {DEFAULT_INSTANCE_TYPE} · image{" "}
              {imageTag} · terminates itself after {maxHours} h
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
                          hours * PLANET_HOURLY_USD,
                        )} of ${DEFAULT_INSTANCE_TYPE}`
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
