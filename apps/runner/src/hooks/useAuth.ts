import { useEffect, useState } from "react";
import {
  type AppConfig,
  beginSignIn,
  CALLBACK_PATH,
  clearSession,
  completeSignIn,
  EXPIRY_SKEW_MS,
  loadAppConfig,
  loadSession,
  type RunsConfig,
  type Session,
  signOut,
} from "@/lib/auth";
import type { LiveConfig } from "@/lib/live";
import type { RunApiConfig } from "@/lib/runApi";

export type AuthState =
  /** Resolving config / finishing the callback exchange. */
  | { status: "loading" }
  /**
   * No auth wiring: plain `pnpm dev` without a local config.json — the local
   * console. `live` points at the run backend `just ui` starts (override with
   * VITE_RUNNER_LIVE_URL, or set it empty for the demo replay alone).
   */
  | { status: "open"; live: LiveConfig | null }
  | { status: "signed-out"; signIn: () => void }
  | {
      status: "signed-in";
      session: Session;
      signOut: () => void;
      /** Drop the expired session and go straight back through the hosted UI. */
      reauth: () => void;
      /** Pool ids the live-runs client federates through. */
      userPoolProvider: string;
      runs: RunsConfig | null;
      /** The SSE backend, when this deploy advertises one. */
      live: LiveConfig | null;
      /** The dispatch API, when this deploy advertises one. */
      runApi: RunApiConfig | null;
    }
  | { status: "error"; message: string };

/**
 * Resolve the console's auth state once at boot. In production the deployed
 * config.json is always present, so a missing one fails closed rather than
 * quietly serving an unauthenticated console; dev without config runs open.
 */
export function useAuth(): AuthState {
  const [state, setState] = useState<AuthState>({ status: "loading" });

  useEffect(() => {
    let cancelled = false;
    const apply = (next: AuthState) => {
      if (!cancelled) setState(next);
    };

    (async () => {
      const config = await loadAppConfig();
      if (!config) {
        apply(
          import.meta.env.DEV
            ? { status: "open", live: localLiveConfig() }
            : { status: "error", message: "Auth configuration is missing from this deploy." },
        );
        return;
      }
      if (location.pathname === CALLBACK_PATH) {
        try {
          apply(signedIn(config, await completeSignIn(config.auth)));
        } catch (err) {
          history.replaceState(null, "", "/");
          apply({ status: "error", message: err instanceof Error ? err.message : String(err) });
        }
        return;
      }
      const session = loadSession();
      apply(
        session
          ? signedIn(config, session)
          : { status: "signed-out", signIn: () => void beginSignIn(config.auth) },
      );
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  return state;
}

/**
 * The local run backend, for a dev server with no deployed config: whatever
 * VITE_RUNNER_LIVE_URL says, or the port `just ui-server` listens on. Set the
 * variable empty to run the console with no backend at all.
 */
function localLiveConfig(): LiveConfig | null {
  const url = (import.meta.env.VITE_RUNNER_LIVE_URL ?? "http://localhost:8080").trim();
  return url ? { url } : null;
}

/** How often to re-check a session that was only validated at boot. */
const EXPIRY_POLL_MS = 60_000;

/**
 * Whether the id token has lapsed mid-session. Without this the first symptom
 * is a 401 the SSE stream hides behind its reconnect loop (lib/live.ts:85).
 */
export function useSessionExpired(auth: AuthState): boolean {
  const expiresAt = auth.status === "signed-in" ? auth.session.expiresAt : null;
  const [expired, setExpired] = useState(false);

  useEffect(() => {
    if (expiresAt === null) return;
    const check = () => setExpired(Date.now() >= expiresAt - EXPIRY_SKEW_MS);
    check();
    const timer = setInterval(check, EXPIRY_POLL_MS);
    return () => clearInterval(timer);
  }, [expiresAt]);

  return expired;
}

function signedIn(config: AppConfig, session: Session): AuthState {
  return {
    status: "signed-in",
    session,
    signOut: () => signOut(config.auth),
    reauth: () => {
      clearSession();
      void beginSignIn(config.auth);
    },
    userPoolProvider: `cognito-idp.${config.auth.region}.amazonaws.com/${config.auth.userPoolId}`,
    runs: config.runs,
    live: config.live,
    runApi: config.runApi,
  };
}
