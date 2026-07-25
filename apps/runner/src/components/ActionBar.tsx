import { Button } from "@planetiler-rs/ui/button";
import { type ReactNode, useEffect, useState } from "react";
import type { ActionKind, Affordances } from "@/lib/fleet";

/** How long a destroy stays armed before it disarms itself. */
const CONFIRM_MS = 5_000;

const PENDING_LABEL: Record<ActionKind, string> = {
  requisition: "requisitioning…",
  dispatch: "dispatching…",
  cancel: "cancelling…",
  destroy: "destroying…",
};

interface ActionBarProps {
  affordances: Affordances;
  pending?: ActionKind | null;
  error?: string | null;
  onDismissError?: () => void;
  /** Right-hand chip slot — a run chip for the demo, a box chip for a box. */
  status?: ReactNode;
  /** Arm-to-confirm the destroy; the simulated console doesn't need it. */
  confirmDestroy?: boolean;
  onRequisition: () => void;
  onDispatch: () => void;
  onCancel: () => void;
  onDestroy: () => void;
  /** Inline requisition controls beside the button (the job toggle). */
  requisitionControls?: ReactNode;
  /** The rest of the requisition form, rendered under its row. */
  children?: ReactNode;
}

export function ActionBar({
  affordances,
  pending = null,
  error = null,
  onDismissError,
  status,
  confirmDestroy = false,
  onRequisition,
  onDispatch,
  onCancel,
  onDestroy,
  requisitionControls,
  children,
}: ActionBarProps) {
  const [armed, setArmed] = useState(false);
  const busy = pending !== null;

  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setArmed(false), CONFIRM_MS);
    return () => clearTimeout(timer);
  }, [armed]);

  const destroy = () => {
    if (confirmDestroy && !armed) {
      setArmed(true);
      return;
    }
    setArmed(false);
    onDestroy();
  };

  return (
    <div className="flex flex-col gap-2">
      {/* Scope: the selected box. Everything on this row acts on it. */}
      <div className="flex flex-wrap items-center gap-2">
        <Button disabled={!affordances.canDispatch || busy} onClick={onDispatch}>
          {pending === "dispatch" ? PENDING_LABEL.dispatch : "Dispatch job"}
        </Button>
        <Button disabled={!affordances.canCancel || busy} onClick={onCancel}>
          {pending === "cancel" ? PENDING_LABEL.cancel : "Cancel run"}
        </Button>
        <Button
          variant={armed ? "solid" : "ghost"}
          className={
            armed
              ? "border-danger bg-danger text-page hover:opacity-85"
              : "text-danger hover:bg-danger/10 hover:text-danger"
          }
          disabled={!affordances.canDestroy || busy}
          onClick={destroy}
        >
          {pending === "destroy"
            ? PENDING_LABEL.destroy
            : armed
              ? "Confirm destroy"
              : "Destroy box"}
        </Button>
        {armed && (
          <Button variant="ghost" onClick={() => setArmed(false)}>
            Keep it
          </Button>
        )}
        {status && <div className="ml-auto">{status}</div>}
      </div>

      {affordances.note && <p className="text-xs text-ink-muted">{affordances.note}</p>}

      {error && (
        <p className="flex items-start gap-2 text-xs text-danger">
          <span className="flex-1">{error}</span>
          {onDismissError && (
            <button type="button" className="underline" onClick={onDismissError}>
              dismiss
            </button>
          )}
        </p>
      )}

      {/* Scope: a new box. The form describes what Requisition will create,
          separated so it can't read as facts about the selection above. */}
      <div className="flex flex-col gap-2 border-t border-grid pt-3">
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="solid"
            disabled={!affordances.canRequisition || busy}
            onClick={onRequisition}
          >
            {pending === "requisition" ? PENDING_LABEL.requisition : "Requisition box"}
          </Button>
          {requisitionControls}
        </div>
        {children}
      </div>
    </div>
  );
}
