// The fleet side of the console: a polled `GET /boxes` listing plus the four
// mutations behind the ActionBar. Mirrors useLiveConsole's shape — null when
// the console has no dispatch wiring (dev, or a deploy with the kill switch
// on) — and owns no selection: the mutations resolve ids and hand them back.

import { useCallback, useEffect, useRef, useState } from "react";
import type { AuthState } from "@/hooks/useAuth";
import type { ActionKind } from "@/lib/fleet";
import {
  type BoxView,
  cancelJob,
  type DispatchRequest,
  destroyBox,
  dispatchJob,
  listBoxes,
  type RequisitionRequest,
  RunApiError,
  requisitionBox,
} from "@/lib/runApi";

/** Fast enough to watch a requisition reach `running` (30-60 s), cheap enough
 * to leave open all day: one warm Lambda and two DescribeInstances a tick. */
const LIST_POLL_MS = 15_000;
/** EC2 and SSM lag their own write APIs; look again once the dust settles. */
const SETTLE_MS = 3_000;
const EXPIRED_MESSAGE = "session expired — sign in again to keep dispatching";

export interface Fleet {
  /** Null while the first listing is in flight. */
  boxes: BoxView[] | null;
  /** Listing failure; the last good list is kept on screen. */
  error: string | null;
  /** Last mutation failure, until dismissed or superseded. */
  actionError: string | null;
  pending: ActionKind | null;
  /** Destroyed but still listed — EC2 reports `shutting-down` for a while. */
  terminating: ReadonlySet<string>;
  /** The API answered 401: the poll has stopped and the token needs renewing. */
  expired: boolean;
  /** Resolves the new box id, or null when the requisition failed. */
  requisition: (request: RequisitionRequest) => Promise<string | null>;
  /** Resolves the new run id, or null when the dispatch failed. */
  dispatch: (boxId: string, request?: DispatchRequest) => Promise<string | null>;
  cancel: (boxId: string) => Promise<void>;
  destroy: (boxId: string) => Promise<void>;
  refresh: () => void;
  dismissError: () => void;
}

export function useFleet(auth: AuthState): Fleet | null {
  const config = auth.status === "signed-in" ? auth.runApi : null;
  const idToken = auth.status === "signed-in" ? auth.session.idToken : "";

  const [boxes, setBoxes] = useState<BoxView[] | null>(null);
  const [optimistic, setOptimistic] = useState<BoxView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [pending, setPending] = useState<ActionKind | null>(null);
  const [terminating, setTerminating] = useState<ReadonlySet<string>>(new Set());
  const [expired, setExpired] = useState(false);

  const inFlight = useRef(false);
  const lifetime = useRef<AbortController | null>(null);
  const timers = useRef<number[]>([]);

  useEffect(() => {
    const controller = new AbortController();
    lifetime.current = controller;
    const scheduled = timers.current;
    return () => {
      controller.abort();
      lifetime.current = null;
      for (const timer of scheduled) clearTimeout(timer);
      scheduled.length = 0;
    };
  }, []);

  const refresh = useCallback(async () => {
    if (!config || inFlight.current) return;
    inFlight.current = true;
    const signal = lifetime.current?.signal;
    try {
      const next = await listBoxes(config, idToken, signal);
      setBoxes(next);
      setError(null);
      // The optimistic row has served its purpose once the listing has it.
      setOptimistic((current) =>
        current && next.some((box) => box.id === current.id) ? null : current,
      );
      setTerminating((current) => {
        if (current.size === 0) return current;
        const kept = new Set<string>();
        for (const id of current) {
          const box = next.find((candidate) => candidate.id === id);
          if (box && box.state !== "shutting-down") kept.add(id);
        }
        return kept.size === current.size ? current : kept;
      });
    } catch (err) {
      if (signal?.aborted) return;
      if (err instanceof RunApiError && err.kind === "auth") {
        setExpired(true);
        setError(EXPIRED_MESSAGE);
        return;
      }
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      inFlight.current = false;
    }
  }, [config, idToken]);

  useEffect(() => {
    // A dead token would only hammer the gateway; the banner takes over.
    if (!config || expired) return;
    void refresh();
    const timer = setInterval(() => {
      if (!document.hidden) void refresh();
    }, LIST_POLL_MS);
    const onVisible = () => {
      if (!document.hidden) void refresh();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [config, expired, refresh]);

  const act = useCallback(
    async <T>(kind: ActionKind, run: () => Promise<T>): Promise<T | null> => {
      if (!config) return null;
      setPending(kind);
      setActionError(null);
      try {
        const result = await run();
        void refresh();
        timers.current.push(window.setTimeout(() => void refresh(), SETTLE_MS));
        return result;
      } catch (err) {
        if (err instanceof RunApiError && err.kind === "auth") {
          setExpired(true);
          setActionError(EXPIRED_MESSAGE);
        } else {
          setActionError(err instanceof Error ? err.message : String(err));
        }
        return null;
      } finally {
        setPending(null);
      }
    },
    [config, refresh],
  );

  const requisition = useCallback(
    async (request: RequisitionRequest) => {
      if (!config) return null;
      const created = await act("requisition", () => requisitionBox(config, idToken, request));
      if (!created) return null;
      // Show the box the moment it exists: DescribeInstances is eventually
      // consistent, and the most expensive button here should not look inert.
      setOptimistic({
        id: created.id,
        region: created.region,
        state: "pending",
        instanceType: created.instanceType,
        launchTime: new Date().toISOString(),
        publicIp: null,
        job: created.job,
        image: created.imageTag,
        runId: null,
        dispatch: "console",
        ready: null,
        maxHours: created.maxHours,
        eventsUrl: null,
      });
      return created.id;
    },
    [act, config, idToken],
  );

  const dispatch = useCallback(
    async (boxId: string, request: DispatchRequest = {}) => {
      if (!config) return null;
      const started = await act("dispatch", () => dispatchJob(config, idToken, boxId, request));
      return started?.runId ?? null;
    },
    [act, config, idToken],
  );

  const cancel = useCallback(
    async (boxId: string) => {
      if (!config) return;
      await act("cancel", () => cancelJob(config, idToken, boxId));
    },
    [act, config, idToken],
  );

  const destroy = useCallback(
    async (boxId: string) => {
      if (!config) return;
      const done = await act("destroy", () => destroyBox(config, idToken, boxId).then(() => true));
      if (done) setTerminating((current) => new Set(current).add(boxId));
    },
    [act, config, idToken],
  );

  const dismissError = useCallback(() => setActionError(null), []);

  if (!config) return null;
  return {
    boxes: withOptimistic(boxes, optimistic),
    error,
    actionError,
    pending,
    terminating,
    expired,
    requisition,
    dispatch,
    cancel,
    destroy,
    refresh: () => void refresh(),
    dismissError,
  };
}

function withOptimistic(boxes: BoxView[] | null, optimistic: BoxView | null): BoxView[] | null {
  if (!optimistic) return boxes;
  if (!boxes) return [optimistic];
  return boxes.some((box) => box.id === optimistic.id) ? boxes : [optimistic, ...boxes];
}
