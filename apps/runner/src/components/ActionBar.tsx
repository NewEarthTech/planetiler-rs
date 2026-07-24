import { Button } from "@planetiler-rs/ui/button";
import { StatusChip } from "@/components/StatusChip";
import type { RunState } from "@/hooks/useDemoRun";

interface ActionBarProps {
  state: RunState;
  onRequisition: () => void;
  onDispatch: () => void;
  onCancel: () => void;
  onDestroy: () => void;
}

export function ActionBar({
  state,
  onRequisition,
  onDispatch,
  onCancel,
  onDestroy,
}: ActionBarProps) {
  const canDispatch = state === "ready" || state === "done" || state === "cancelled";
  return (
    <div className="flex flex-wrap items-center gap-2">
      <Button variant="solid" disabled={state !== "idle"} onClick={onRequisition}>
        Requisition box
      </Button>
      <Button disabled={!canDispatch} onClick={onDispatch}>
        Dispatch job
      </Button>
      <Button disabled={state !== "running"} onClick={onCancel}>
        Cancel run
      </Button>
      <Button
        variant="ghost"
        className="text-danger hover:bg-danger/10 hover:text-danger"
        disabled={state === "idle"}
        onClick={onDestroy}
      >
        Destroy box
      </Button>
      <div className="ml-auto">
        <StatusChip state={state} />
      </div>
    </div>
  );
}
