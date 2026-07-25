// What the console is allowed to do to a box, and how to label it. Pure
// derivation over the API's own vocabulary (raw EC2 states and the four tags
// crates/run-api writes), kept out of the components so the rules read as
// rules. RunState stays the demo machine's alphabet — a box lifecycle is a
// different thing and gets its own chip.

import type { BadgeProps } from "@planetiler-rs/ui/badge";
import { fmtDuration } from "@planetiler-rs/ui/utils";
import type { RunState } from "@/lib/feed";
import type { BoxView } from "@/lib/runApi";

export type ActionKind = "requisition" | "dispatch" | "cancel" | "destroy";

export interface Affordances {
  canRequisition: boolean;
  canDispatch: boolean;
  canCancel: boolean;
  canDestroy: boolean;
  /** One line saying whatever the disabled buttons can't say themselves. */
  note: string | null;
}

/** The only job run-api will requisition or dispatch today (jobs.rs:60-62). */
export const DISPATCHABLE_JOB = "planet";

/** Boxes this console launched; only those carry the .box-ready marker. */
function isConsoleBox(box: BoxView): boolean {
  return box.dispatch === "console";
}

/**
 * The gating for a selected box, or for none at all — requisition is never
 * gated on a selection, since it is what creates one.
 *
 * Dispatch and cancel are refused for laptop-dispatched boxes even though the
 * server would accept them: run-api checks only the job tag, but the SSM
 * script waits for /home/ubuntu/.box-ready, which only console requisition
 * writes (commands.rs:79-80, userdata.rs:48). A dispatch there returns 202 and
 * then burns ~15 minutes on the box before failing, so this check is the only
 * guard that exists today.
 */
export function boxAffordances(box: BoxView | null, terminating: boolean): Affordances {
  if (!box) {
    return {
      canRequisition: true,
      canDispatch: false,
      canCancel: false,
      canDestroy: false,
      note: null,
    };
  }
  const ours = isConsoleBox(box);
  const going = terminating || box.state === "shutting-down";
  const running = box.state === "running";
  return {
    canRequisition: true,
    canDispatch: ours && box.job === DISPATCHABLE_JOB && running && !going,
    // Left enabled after a job finishes: the run-id tag outlives the run and
    // the console has no cheap completion signal, so cancelling a finished
    // run is a pkill of nothing rather than something worth gating on.
    canCancel: ours && running && box.runId !== null && !going,
    canDestroy: !going,
    note: boxNote(box, going, ours, running),
  };
}

function boxNote(box: BoxView, going: boolean, ours: boolean, running: boolean): string | null {
  if (going) return "terminating — the row clears once EC2 finishes.";
  if (!ours) {
    return "laptop-dispatched box: watch and destroy only. Console dispatch waits on a marker that only its own requisition writes.";
  }
  if (box.job && box.job !== DISPATCHABLE_JOB) {
    return "A/B jobs need a branch-built candidate image — dispatch them from the laptop path until image.yml builds those.";
  }
  if (!box.job) return "no job tag: the API will refuse a dispatch.";
  if (box.state === "pending") return "still booting — the image pull takes about a minute.";
  if (box.state === "stopping" || box.state === "stopped") {
    return "stopped: the console can only destroy it.";
  }
  if (running && box.runId === null) return "no job dispatched yet.";
  return null;
}

/** Today's ActionBar gating, unchanged, for the demo replay. */
export function demoAffordances(state: RunState): Affordances {
  return {
    canRequisition: state === "idle",
    canDispatch: state === "ready" || state === "done" || state === "cancelled",
    canCancel: state === "running",
    canDestroy: state !== "idle",
    note: null,
  };
}

export interface BoxChipSpec {
  tone: NonNullable<BadgeProps["tone"]>;
  label: string;
  pulse?: boolean;
}

/** The EC2 lifecycle, as a chip. `state` is a raw string, so default neutral. */
export function boxChip(box: BoxView, terminating: boolean): BoxChipSpec {
  if (terminating || box.state === "shutting-down") {
    return { tone: "warn", label: "terminating", pulse: true };
  }
  if (box.state === "pending") return { tone: "warn", label: "booting", pulse: true };
  if (box.state === "stopping" || box.state === "stopped") {
    return { tone: "neutral", label: box.state };
  }
  if (box.state === "running") {
    return box.runId
      ? { tone: "info", label: "job dispatched", pulse: true }
      : { tone: "good", label: "idle — ready" };
  }
  return { tone: "neutral", label: box.state };
}

/** How long the box has been billing, or null before EC2 reports a time. */
export function boxAge(launchTime: string | null, now: number = Date.now()): string | null {
  if (!launchTime) return null;
  const started = Date.parse(launchTime);
  if (Number.isNaN(started)) return null;
  return fmtDuration(Math.max(0, (now - started) / 1000));
}
