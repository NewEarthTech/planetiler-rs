import { useEffect, useState } from "react";
import {
  type AppConfig,
  beginSignIn,
  CALLBACK_PATH,
  completeSignIn,
  loadAppConfig,
  loadSession,
  type RunsConfig,
  type Session,
  signOut,
} from "@/lib/auth";
import type { LiveConfig } from "@/lib/live";

export type AuthState =
  /** Resolving config / finishing the callback exchange. */
  | { status: "loading" }
  /** No auth wiring (plain `pnpm dev` without a local config.json). */
  | { status: "open" }
  | { status: "signed-out"; signIn: () => void }
  | {
      status: "signed-in";
      session: Session;
      signOut: () => void;
      /** Pool ids the live-runs client federates through. */
      userPoolProvider: string;
      runs: RunsConfig | null;
      /** The SSE backend, when this deploy advertises one. */
      live: LiveConfig | null;
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
            ? { status: "open" }
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

function signedIn(config: AppConfig, session: Session): AuthState {
  return {
    status: "signed-in",
    session,
    signOut: () => signOut(config.auth),
    userPoolProvider: `cognito-idp.${config.auth.region}.amazonaws.com/${config.auth.userPoolId}`,
    runs: config.runs,
    live: config.live,
  };
}
