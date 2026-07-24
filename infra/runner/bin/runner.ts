#!/usr/bin/env node
import "source-map-support/register";
import * as path from "path";
import * as dotenv from "dotenv";
import * as cdk from "aws-cdk-lib";
import { RunnerConsoleStack } from "../lib/site-stack";

// Load infra/runner/.env (gitignored) before reading config, regardless of cwd.
dotenv.config({ path: path.join(__dirname, "..", ".env") });

const app = new cdk.App();

// Whatever account your AWS credentials point at. No account id is baked in.
const account = process.env.CDK_DEFAULT_ACCOUNT;

cdk.Tags.of(app).add("Project", "planetiler-runner");
cdk.Tags.of(app).add("ManagedBy", "cdk");

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
});

function requireEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`${name} is required — set it in infra/runner/.env (see .env.example)`);
  }
  return value;
}
