#!/usr/bin/env node
import "source-map-support/register";
import * as fs from "fs";
import * as path from "path";
import * as dotenv from "dotenv";
import * as cdk from "aws-cdk-lib";
import { RunApiStack } from "../lib/api-stack";
import { RunnerBackendStack } from "../lib/backend-stack";
import { RunnerConsoleStack } from "../lib/site-stack";

// Load infra/runner/.env (gitignored) before reading config, regardless of cwd.
dotenv.config({ path: path.join(__dirname, "..", ".env") });

const app = new cdk.App();

// Whatever account your AWS credentials point at. No account id is baked in.
const account = process.env.CDK_DEFAULT_ACCOUNT;

cdk.Tags.of(app).add("Project", "planetiler-runner");
cdk.Tags.of(app).add("ManagedBy", "cdk");

// The SSE backend's domain, shared by both stacks: RunnerBackendStack serves
// it, RunnerConsoleStack advertises it in config.json. Explicitly empty
// ("" in .env or the workflow) disables the advert — the kill switch that
// drops the console back to CloudWatch polling on the next deploy.
const liveDomain = process.env.RUNNER_LIVE_DOMAIN ?? "live.runner.johncarmack.com";

// The dispatch API's domain, same pattern: RunApiStack serves it,
// RunnerConsoleStack advertises it. "" = no dispatch controls in the console.
const apiDomain = process.env.RUNNER_API_DOMAIN ?? "api.runner.johncarmack.com";

// Each stack only instantiates when its build artifact exists, because
// synthesizing this app synthesizes every stack in it: the backend workflow
// deploys RunnerBackendStack without ever building the console bundle, and
// console deploys don't cross-build the Rust binary. Missing artifact =
// that stack is simply absent from this synth, with a warning.
const consoleDist = path.join(__dirname, "..", "..", "..", "apps", "runner", "dist");
if (fs.existsSync(consoleDist)) {
  new RunnerConsoleStack(app, "RunnerConsoleStack", {
    // CloudFront and its ACM certificate must live in us-east-1.
    env: { account, region: "us-east-1" },
    domainName: process.env.RUNNER_DOMAIN ?? "runner.johncarmack.com",
    zoneName: process.env.RUNNER_ZONE_NAME ?? "johncarmack.com",
    // The parent hosted zone id, from infra/runner/.env (kept out of the repo).
    zoneId: requireEnv("RUNNER_ZONE_ID"),
    authDomainPrefix: process.env.RUNNER_AUTH_DOMAIN_PREFIX ?? "runner-johncarmack",
    // Empty and unset both mean "not wired yet" (CI passes the repo variable
    // through even when it is blank).
    samlMetadataUrl: process.env.RUNNER_SAML_METADATA_URL || undefined,
    // Only used before SAML is wired; defaults to the repo's commit author.
    ownerEmail: process.env.RUNNER_OWNER_EMAIL ?? "johncarmack@me.com",
    liveUrl: liveDomain ? `https://${liveDomain}` : undefined,
    runApiUrl: apiDomain ? `https://${apiDomain}` : undefined,
  });
} else {
  console.warn(`skipping RunnerConsoleStack: ${consoleDist} not found (just runner::build)`);
}

// The dispatch API (#59). Only instantiated when the Lambda bootstrap is
// present — same guarded-synth rule as the other two build artifacts.
const apiDist = path.join(__dirname, "..", "dist-api");
if (fs.existsSync(path.join(apiDist, "bootstrap")) && apiDomain) {
  new RunApiStack(app, "RunApiStack", {
    env: { account, region: "us-east-1" },
    domainName: apiDomain,
    zoneName: process.env.RUNNER_ZONE_NAME ?? "johncarmack.com",
    zoneId: requireEnv("RUNNER_ZONE_ID"),
    consoleOrigin: `https://${process.env.RUNNER_DOMAIN ?? "runner.johncarmack.com"}`,
    liveBase: liveDomain ? `https://${liveDomain}` : "",
    distDir: apiDist,
  });
} else {
  console.warn(`skipping RunApiStack: ${apiDist}/bootstrap not found (just runner::build-api)`);
}

// The SSE backend (#59). Only instantiated when the cross-built binary is
// present, so console-only deploys never require a Rust build — the binary
// comes from `just runner::build-backend` locally or the backend workflow.
const backendBinary = path.join(__dirname, "..", "dist-backend", "runner-live");
if (fs.existsSync(backendBinary) && liveDomain) {
  new RunnerBackendStack(app, "RunnerBackendStack", {
    env: { account, region: "us-east-1" },
    domainName: liveDomain,
    zoneName: process.env.RUNNER_ZONE_NAME ?? "johncarmack.com",
    zoneId: requireEnv("RUNNER_ZONE_ID"),
    binaryPath: backendBinary,
    consoleConfigUrl: `https://${process.env.RUNNER_DOMAIN ?? "runner.johncarmack.com"}/config.json`,
    acmeEmail: process.env.RUNNER_OWNER_EMAIL ?? "johncarmack@me.com",
  });
} else {
  console.warn(
    `skipping RunnerBackendStack: ${backendBinary} not found (just runner::build-backend)`,
  );
}

function requireEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`${name} is required — set it in infra/runner/.env (see .env.example)`);
  }
  return value;
}
