// Read side of the results bucket: what a finished run shipped home. The box
// uploads results/ plus the final logs to s3://<bucket>/<run-id>/ on DONE and
// on FAIL (services/cloud-runner/remote-planet.sh), so this is keyed by run id
// — a run whose box is long gone still has artifacts. The identity pool's
// authenticated role already carries GetObject + ListBucket, and the bucket
// already allows GET/HEAD from the console origin, so nothing new is granted.
//
// Loaded through a dynamic import: the S3 client and the presigner are dead
// weight in the bundle until a run is actually selected.

import { GetObjectCommand, ListObjectsV2Command, S3Client } from "@aws-sdk/client-s3";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";
import type { RunsConfig } from "@/lib/auth";
import { makeCredentials } from "@/lib/runs";

export interface ResultObject {
  key: string;
  /** Path under the run prefix, e.g. "results/planet-20260722-031500.txt". */
  name: string;
  size: number;
  lastModified: number | null;
  /** Presigned, so the browser downloads it by navigation, not by buffering. */
  url: string;
}

/**
 * Long enough for a session at the console, short enough to be worthless if
 * copied; the identity-pool credentials behind it expire within the hour
 * regardless.
 */
const URL_TTL_S = 900;
/** The report is small and is the point; anything larger stays a download. */
export const PREVIEW_MAX_BYTES = 64 * 1024;

export function makeResultsClient(
  runs: RunsConfig,
  userPoolProvider: string,
  idToken: string,
): S3Client {
  return new S3Client({
    region: runs.region,
    credentials: makeCredentials(runs, userPoolProvider, idToken),
  });
}

/** Everything this run shipped, report first, raw artifacts last. */
export async function listResults(
  client: S3Client,
  bucket: string,
  runId: string,
): Promise<ResultObject[]> {
  const listed = await client.send(
    new ListObjectsV2Command({ Bucket: bucket, Prefix: `${runId}/` }),
  );
  const objects = (listed.Contents ?? []).filter((object) => object.Key && (object.Size ?? 0) > 0);
  const results = await Promise.all(
    objects.map(async (object) => {
      const key = object.Key as string;
      const name = key.slice(runId.length + 1);
      return {
        key,
        name,
        size: object.Size ?? 0,
        lastModified: object.LastModified?.getTime() ?? null,
        url: await getSignedUrl(
          client,
          new GetObjectCommand({
            Bucket: bucket,
            Key: key,
            ResponseContentDisposition: `attachment; filename="${filename(name)}"`,
          }),
          { expiresIn: URL_TTL_S },
        ),
      };
    }),
  );
  return results.sort((a, b) => rank(a.name) - rank(b.name) || a.name.localeCompare(b.name));
}

/** The run report, for rendering inline. */
export async function fetchText(client: S3Client, bucket: string, key: string): Promise<string> {
  const object = await client.send(new GetObjectCommand({ Bucket: bucket, Key: key }));
  return (await object.Body?.transformToString()) ?? "";
}

/** The human-readable report, if this run shipped one. */
export function findReport(results: ResultObject[]): ResultObject | null {
  return (
    results.find((result) => isReport(result.name) && result.size <= PREVIEW_MAX_BYTES) ?? null
  );
}

function isReport(name: string): boolean {
  return name.startsWith("results/") && name.endsWith(".txt") && !name.includes("tiles-delta");
}

function rank(name: string): number {
  if (isReport(name)) return 0;
  if (name === "results/tiles-delta.txt") return 1;
  if (name === "job.log") return 2;
  if (name.endsWith(".log") || name.endsWith(".stderr")) return 3;
  return 4;
}

function filename(name: string): string {
  return name.replaceAll("/", "-").replace(/[^\w.-]/g, "_");
}
