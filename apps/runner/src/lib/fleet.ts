// What the console is allowed to do to a box, and how to label it. Pure
// derivation over the API's own vocabulary (raw EC2 states and the tags
// crates/run-api writes), kept out of the components so the rules read as
// rules.

import type { BadgeProps } from "@planetiler-rs/ui/badge";
import { fmtDuration } from "@planetiler-rs/ui/utils";
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

/**
 * The jobs run-api will requisition or dispatch (jobs.rs `dispatchable`):
 * the real planet run, and its monaco-sized smoke rehearsal — same pipeline,
 * same legs, minutes and cents instead of hours and dollars. A/B jobs stay
 * on the laptop path until their dispatch wiring exists.
 */
export const DISPATCHABLE_JOBS = ["planet", "planet-smoke"] as const;
export type DispatchableJob = (typeof DISPATCHABLE_JOBS)[number];

export function isDispatchableJob(job: string | null): job is DispatchableJob {
  return job !== null && (DISPATCHABLE_JOBS as readonly string[]).includes(job);
}

/** Boxes this console launched; only those carry the .box-ready marker. */
function isConsoleBox(box: BoxView): boolean {
  return box.dispatch === "console";
}

/**
 * The gating for a selected box, or for none at all — requisition is never
 * gated on a selection, since it is what creates one.
 *
 * Dispatch and cancel are refused for laptop-dispatched boxes; run-api 409s
 * them too (only console requisition writes the .box-ready marker the SSM
 * script waits on), so this gating is a legible disabled button in front of
 * the server's own refusal, not the only guard.
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
    canDispatch: ours && isDispatchableJob(box.job) && running && !going,
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
    return "laptop-dispatched box: watch and destroy only. The API refuses to dispatch onto boxes it didn't requisition.";
  }
  if (box.job && !isDispatchableJob(box.job)) {
    return "A/B dispatch isn't wired yet — those runs still go out from the laptop path.";
  }
  if (!box.job) return "no job tag: the API will refuse a dispatch.";
  if (box.state === "pending") return "still booting — the image pull takes about a minute.";
  if (box.state === "stopping" || box.state === "stopped") {
    return "stopped: the console can only destroy it.";
  }
  if (running && !box.ready && box.runId === null) {
    return "still provisioning (image pull + script extraction, ~1 min); a dispatch now waits on the box for readiness.";
  }
  if (running && box.runId === null) return "no job dispatched yet.";
  return null;
}

export interface BoxChipSpec {
  tone: NonNullable<BadgeProps["tone"]>;
  label: string;
  pulse?: boolean;
}

/**
 * The box lifecycle, as a chip. EC2 `running` alone is not readiness — a
 * console box says "provisioning" until the `ready` tag (the .box-ready
 * mirror) appears. Laptop boxes never write it, so a running one without a
 * run-id just says "running": the console can't know whether ssh is busy.
 */
export function boxChip(box: BoxView, terminating: boolean): BoxChipSpec {
  if (terminating || box.state === "shutting-down") {
    return { tone: "warn", label: "terminating", pulse: true };
  }
  if (box.state === "pending") return { tone: "warn", label: "booting", pulse: true };
  if (box.state === "stopping" || box.state === "stopped") {
    return { tone: "neutral", label: box.state };
  }
  if (box.state === "running") {
    if (box.runId) return { tone: "info", label: "job dispatched", pulse: true };
    if (!isConsoleBox(box)) return { tone: "neutral", label: "running" };
    return box.ready
      ? { tone: "good", label: "idle — ready" }
      : { tone: "warn", label: "provisioning", pulse: true };
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
