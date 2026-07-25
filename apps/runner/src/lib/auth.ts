// Browser-side OIDC against the Cognito hosted UI: authorization code +
// PKCE, no client secret, no SDK. The wiring (domain, client id) comes from
// /config.json, published next to the bundle by the CDK stack — absent in
// plain `pnpm dev`, where the console runs unauthenticated.
//
// Deliberately no refresh flow: tokens last 12 h (set on the pool client),
// the session lives in sessionStorage, and an expired session just routes
// back through the hosted UI — a silent redirect while the AWS access portal
// session is alive.

import type { LiveConfig } from "@/lib/live";
import type { RunApiConfig } from "@/lib/runApi";

export interface AuthConfig {
  /** Hosted UI base URL, e.g. https://runner-johncarmack.auth.us-east-1.amazoncognito.com */
  domain: string;
  clientId: string;
  userPoolId: string;
  region: string;
  /** SAML provider name once federated to Identity Center; "" before that. */
  identityProvider: string;
}

/** Where the live run logs live; published in config.json by the CDK stack. */
export interface RunsConfig {
  region: string;
  logGroup: string;
  identityPoolId: string;
  /** Where finished runs ship their artifacts; null on deploys predating it. */
  resultsBucket: string | null;
}

export interface AppConfig {
  auth: AuthConfig;
  /** Absent on deploys older than the run-logs infra. */
  runs: RunsConfig | null;
  /** The SSE backend (#59); absent when the deploy doesn't advertise one, in which case the live view falls back to CloudWatch polling. */
  live: LiveConfig | null;
  /** The dispatch API (#59); absent means no console-side dispatch controls. */
  runApi: RunApiConfig | null;
}

export interface Session {
  idToken: string;
  accessToken: string;
  /** Epoch milliseconds. */
  expiresAt: number;
  email: string;
}

export const CALLBACK_PATH = "/auth/callback";
const PKCE_KEY = "runner-auth-pkce";
const SESSION_KEY = "runner-auth-session";
/** Treat sessions expiring within a minute as already gone. */
export const EXPIRY_SKEW_MS = 60_000;

/**
 * Fetch /config.json and return its auth + runs blocks, or null when it's
 * missing or not JSON (the SPA-fallback CloudFront config answers unknown
 * paths with index.html, so a missing file arrives as HTML with status 200).
 */
export async function loadAppConfig(): Promise<AppConfig | null> {
  try {
    const res = await fetch("/config.json", { cache: "no-cache" });
    if (!res.ok) return null;
    const parsed: unknown = JSON.parse(await res.text());
    const { auth, runs, live, runApi } = parsed as {
      auth?: Partial<AuthConfig>;
      runs?: Partial<RunsConfig>;
      live?: Partial<LiveConfig>;
      runApi?: Partial<RunApiConfig>;
    };
    if (!auth || typeof auth.domain !== "string" || typeof auth.clientId !== "string") {
      return null;
    }
    return {
      auth: {
        domain: auth.domain,
        clientId: auth.clientId,
        userPoolId: auth.userPoolId ?? "",
        region: auth.region ?? "",
        identityProvider: auth.identityProvider ?? "",
      },
      runs:
        runs &&
        typeof runs.region === "string" &&
        typeof runs.logGroup === "string" &&
        typeof runs.identityPoolId === "string"
          ? {
              region: runs.region,
              logGroup: runs.logGroup,
              identityPoolId: runs.identityPoolId,
              resultsBucket: typeof runs.resultsBucket === "string" ? runs.resultsBucket : null,
            }
          : null,
      live: live && typeof live.url === "string" && live.url ? { url: live.url } : null,
      runApi: runApi && typeof runApi.url === "string" && runApi.url ? { url: runApi.url } : null,
    };
  } catch {
    return null;
  }
}

/** Redirect to the hosted UI's authorize endpoint (never returns normally). */
export async function beginSignIn(config: AuthConfig): Promise<void> {
  const verifier = randomToken();
  const state = randomToken();
  sessionStorage.setItem(PKCE_KEY, JSON.stringify({ verifier, state }));

  const params = new URLSearchParams({
    client_id: config.clientId,
    response_type: "code",
    scope: "openid email profile",
    redirect_uri: location.origin + CALLBACK_PATH,
    state,
    code_challenge: base64Url(await sha256(verifier)),
    code_challenge_method: "S256",
  });
  // Skip the hosted UI's provider chooser and go straight to the portal.
  if (config.identityProvider) params.set("identity_provider", config.identityProvider);
  location.assign(`${config.domain}/oauth2/authorize?${params}`);
}

// React StrictMode mounts effects twice in dev; an authorization code is
// single-use, so both mounts must share one exchange.
let exchangeInFlight: Promise<Session> | null = null;

/**
 * Handle the hosted UI redirect back to CALLBACK_PATH: verify state, trade
 * the code for tokens, persist the session, and clean the URL.
 */
export function completeSignIn(config: AuthConfig): Promise<Session> {
  exchangeInFlight ??= (async () => {
    const query = new URLSearchParams(location.search);
    const oauthError = query.get("error");
    if (oauthError) {
      throw new Error(query.get("error_description") ?? `sign-in failed: ${oauthError}`);
    }
    const code = query.get("code");
    const state = query.get("state");
    const pkce = sessionStorage.getItem(PKCE_KEY);
    sessionStorage.removeItem(PKCE_KEY);
    if (!code || !state || !pkce) throw new Error("sign-in response is missing its code or state");
    const { verifier, state: expectedState } = JSON.parse(pkce) as {
      verifier: string;
      state: string;
    };
    if (state !== expectedState) throw new Error("sign-in state mismatch — please try again");

    const res = await fetch(`${config.domain}/oauth2/token`, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "authorization_code",
        client_id: config.clientId,
        code,
        redirect_uri: location.origin + CALLBACK_PATH,
        code_verifier: verifier,
      }),
    });
    if (!res.ok) throw new Error(`token exchange failed (${res.status})`);
    const tokens = (await res.json()) as {
      id_token: string;
      access_token: string;
      expires_in: number;
    };

    const session: Session = {
      idToken: tokens.id_token,
      accessToken: tokens.access_token,
      expiresAt: Date.now() + tokens.expires_in * 1000,
      email: emailFromIdToken(tokens.id_token),
    };
    sessionStorage.setItem(SESSION_KEY, JSON.stringify(session));
    history.replaceState(null, "", "/");
    return session;
  })();
  return exchangeInFlight;
}

/** Drop the stored session, leaving the hosted-UI session alone. */
export function clearSession(): void {
  sessionStorage.removeItem(SESSION_KEY);
}

/** The stored session, or null when absent or expiring within the skew. */
export function loadSession(): Session | null {
  const raw = sessionStorage.getItem(SESSION_KEY);
  if (!raw) return null;
  try {
    const session = JSON.parse(raw) as Session;
    if (session.expiresAt - EXPIRY_SKEW_MS <= Date.now()) {
      clearSession();
      return null;
    }
    return session;
  } catch {
    clearSession();
    return null;
  }
}

/** Clear the session and end the hosted-UI session too (never returns normally). */
export function signOut(config: AuthConfig): void {
  clearSession();
  const params = new URLSearchParams({
    client_id: config.clientId,
    logout_uri: `${location.origin}/`,
  });
  location.assign(`${config.domain}/logout?${params}`);
}

function emailFromIdToken(idToken: string): string {
  try {
    const payload = idToken.split(".")[1];
    const json = atob(payload.replace(/-/g, "+").replace(/_/g, "/"));
    const claims = JSON.parse(json) as { email?: string; "cognito:username"?: string };
    return claims.email ?? claims["cognito:username"] ?? "";
  } catch {
    return "";
  }
}

function randomToken(): string {
  return base64Url(crypto.getRandomValues(new Uint8Array(32)).buffer);
}

async function sha256(text: string): Promise<ArrayBuffer> {
  return crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
}

function base64Url(bytes: ArrayBuffer): string {
  return btoa(String.fromCharCode(...new Uint8Array(bytes)))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}
