// Client for the dispatch API (#59): requisition a bench box, dispatch a job
// onto it, cancel it, destroy it, list the fleet. The contract of record is
// crates/run-api/src/server.rs — including the input allowlists, mirrored
// here so a typo is refused in the form instead of round-tripping a 400.
// Dependency-free like lib/live.ts: nothing in here knows about React or AWS.

/** Where the dispatch API lives; published in config.json by the CDK stack. */
export interface RunApiConfig {
  /** e.g. https://api.runner.planetiler.rs */
  url: string;
}

/** One bench box as `GET /boxes` reports it; every key is always present. */
export interface BoxView {
  id: string;
  region: string;
  /** Raw EC2 state: pending | running | shutting-down | stopping | stopped. */
  state: string;
  instanceType: string;
  /** RFC3339, or null before EC2 reports it. */
  launchTime: string | null;
  publicIp: string | null;
  job: string | null;
  image: string | null;
  runId: string | null;
  /** "console" for boxes this API launched; null on laptop dispatches. */
  dispatch: string | null;
  eventsUrl: string | null;
}

export interface RequisitionRequest {
  job: string;
  imageTag?: string;
  instanceType?: string;
  maxHours?: number;
  dataEbsGb?: number;
}

export interface RequisitionResponse {
  id: string;
  region: string;
  job: string;
  instanceType: string;
  imageTag: string;
  maxHours: number;
}

/**
 * `planetUrl` is deliberately absent: it is the reproducibility pin behind
 * every number in bench/results/, the server default is the right one, and a
 * field for it next to a $2/h button is an invitation to invalidate a
 * benchmark. Dispatch it from the laptop path if it ever needs changing.
 */
export interface DispatchRequest {
  /** Report label for the run; defaults server-side to the box's image tag. */
  ref?: string;
  maxHours?: number;
}

export interface DispatchResponse {
  runId: string;
  commandId: string;
  eventsUrl: string;
}

export type RunApiErrorKind =
  /** 401/403 — the id token is missing, expired, or for the wrong audience. */
  | "auth"
  /** 400/415/422 — the request itself was refused. */
  | "request"
  | "notFound"
  | "conflict"
  /** 5xx — the API's own AWS calls failed, or the gateway did. */
  | "upstream"
  /** No answer at all: offline, CORS, or the 30 s timeout. */
  | "network";

export class RunApiError extends Error {
  readonly status: number;
  readonly kind: RunApiErrorKind;

  constructor(status: number, kind: RunApiErrorKind, message: string) {
    super(message);
    this.name = "RunApiError";
    this.status = status;
    this.kind = kind;
  }
}

/** Matches check_ident in crates/run-api/src/commands.rs. */
const IDENT_RE = /^[A-Za-z0-9._/:-]{1,128}$/;
export const MAX_HOURS_MIN = 1;
export const MAX_HOURS_MAX = 24;

/** The server's own wording, so a form error and a 400 read the same. */
export function checkIdent(value: string, what: string): string | null {
  if (value.length < 1 || value.length > 128) return `${what} must be 1-128 characters`;
  if (!IDENT_RE.test(value)) return `${what} may only contain [A-Za-z0-9._/:-]`;
  return null;
}

export function checkMaxHours(hours: number): string | null {
  if (!Number.isInteger(hours) || hours < MAX_HOURS_MIN || hours > MAX_HOURS_MAX) {
    return `maxHours must be ${MAX_HOURS_MIN}-${MAX_HOURS_MAX}`;
  }
  return null;
}

/** The Lambda's own ceiling — waiting longer only hides a failure. */
const TIMEOUT_MS = 30_000;
const TIMEOUT_MESSAGE =
  "the dispatch API did not answer in 30 s — the box may still have launched, so refresh the fleet before retrying";

/**
 * The API answers in three envelopes: `{"error": …}` from the app itself,
 * `{"message": …}` from the API Gateway authorizer (401/403), and bare text
 * from axum's own rejections (malformed JSON, wrong content-type, unknown
 * field). Collapse all three into one sentence.
 */
export async function parseError(res: Response): Promise<string> {
  const body = (await res.text().catch(() => "")).trim();
  if (!body) return `HTTP ${res.status}`;
  try {
    const parsed: unknown = JSON.parse(body);
    if (parsed && typeof parsed === "object") {
      const { error, message } = parsed as { error?: unknown; message?: unknown };
      if (typeof error === "string" && error) return error;
      if (typeof message === "string" && message) return message;
    }
  } catch {
    // Not JSON, so it's an axum rejection body: plain text, use it as-is.
  }
  return body.length > 200 ? `${body.slice(0, 200)}…` : body;
}

function kindFor(status: number): RunApiErrorKind {
  if (status === 401 || status === 403) return "auth";
  if (status === 404) return "notFound";
  if (status === 409) return "conflict";
  if (status >= 500) return "upstream";
  return "request";
}

async function call<T>(
  config: RunApiConfig,
  idToken: string,
  method: "GET" | "POST" | "DELETE",
  path: string,
  body?: unknown,
  signal?: AbortSignal,
): Promise<T> {
  const headers: Record<string, string> = { authorization: `Bearer ${idToken}` };
  // axum reads an absent content-type as an absent body, which for dispatch
  // silently means "every default" — so declare it whenever there is a body.
  if (body !== undefined) headers["content-type"] = "application/json";
  const timeout = AbortSignal.timeout(TIMEOUT_MS);

  let res: Response;
  try {
    res = await fetch(`${config.url.replace(/\/+$/, "")}${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      cache: "no-store",
      signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
    });
  } catch (err) {
    if (signal?.aborted) throw err; // the caller went away; not a failure
    const timedOut = err instanceof DOMException && err.name === "TimeoutError";
    throw new RunApiError(
      0,
      "network",
      timedOut ? TIMEOUT_MESSAGE : err instanceof Error ? err.message : String(err),
    );
  }

  if (!res.ok) throw new RunApiError(res.status, kindFor(res.status), await parseError(res));
  return (await res.json()) as T;
}

/** The live fleet across both regions — laptop-dispatched boxes included. */
export async function listBoxes(
  config: RunApiConfig,
  idToken: string,
  signal?: AbortSignal,
): Promise<BoxView[]> {
  const body = await call<{ boxes?: BoxView[] }>(
    config,
    idToken,
    "GET",
    "/boxes",
    undefined,
    signal,
  );
  return body.boxes ?? [];
}

/**
 * Launch a box that provisions itself from the runner image and waits for a
 * dispatch. Bodies are built key by key rather than spread: the request
 * structs are `deny_unknown_fields`, so one stray key is a plain-text 422.
 */
export async function requisitionBox(
  config: RunApiConfig,
  idToken: string,
  request: RequisitionRequest,
  signal?: AbortSignal,
): Promise<RequisitionResponse> {
  const body: Record<string, unknown> = { job: request.job };
  if (request.imageTag) body.imageTag = request.imageTag;
  if (request.instanceType) body.instanceType = request.instanceType;
  if (request.maxHours !== undefined) body.maxHours = request.maxHours;
  if (request.dataEbsGb !== undefined) body.dataEbsGb = request.dataEbsGb;
  return call<RequisitionResponse>(config, idToken, "POST", "/boxes", body, signal);
}

/** Start the box's job over SSM. 202 means SSM took it, not that it ran. */
export async function dispatchJob(
  config: RunApiConfig,
  idToken: string,
  boxId: string,
  request: DispatchRequest = {},
  signal?: AbortSignal,
): Promise<DispatchResponse> {
  const body: Record<string, unknown> = {};
  if (request.ref) body.ref = request.ref;
  if (request.maxHours !== undefined) body.maxHours = request.maxHours;
  try {
    return await call<DispatchResponse>(
      config,
      idToken,
      "POST",
      `/boxes/${encodeURIComponent(boxId)}/jobs`,
      body,
      signal,
    );
  } catch (err) {
    // The likeliest 502 here is SendCommand against a box whose SSM agent
    // hasn't registered yet — worth saying, since the fix is to wait.
    if (err instanceof RunApiError && err.kind === "upstream") {
      throw new RunApiError(
        err.status,
        err.kind,
        `${err.message} — the SSM agent registers a few seconds after a box reaches running; try again`,
      );
    }
    throw err;
  }
}

/** Kill the running job. The box stays up; the boot failsafe still reaps it. */
export async function cancelJob(
  config: RunApiConfig,
  idToken: string,
  boxId: string,
  signal?: AbortSignal,
): Promise<void> {
  await call<{ commandId: string }>(
    config,
    idToken,
    "POST",
    `/boxes/${encodeURIComponent(boxId)}/cancel`,
    undefined,
    signal,
  );
}

export async function destroyBox(
  config: RunApiConfig,
  idToken: string,
  boxId: string,
  signal?: AbortSignal,
): Promise<void> {
  await call<{ id: string; region: string; state: string }>(
    config,
    idToken,
    "DELETE",
    `/boxes/${encodeURIComponent(boxId)}`,
    undefined,
    signal,
  );
}
