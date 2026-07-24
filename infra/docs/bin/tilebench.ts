#!/usr/bin/env node
import "source-map-support/register";
import * as path from "path";
import * as dotenv from "dotenv";
import * as cdk from "aws-cdk-lib";
import { TilebenchStack } from "../lib/site-stack";

// Load infra/docs/.env (gitignored) before reading config, regardless of cwd.
dotenv.config({ path: path.join(__dirname, "..", ".env") });

const app = new cdk.App();

// Whatever account your AWS credentials point at. No account id is baked in.
const account = process.env.CDK_DEFAULT_ACCOUNT;

cdk.Tags.of(app).add("Project", "tilebench");
cdk.Tags.of(app).add("ManagedBy", "cdk");

new TilebenchStack(app, "TilebenchStack", {
  // CloudFront and its ACM certificate must live in us-east-1.
  env: { account, region: "us-east-1" },
  domainName: process.env.TILEBENCH_DOMAIN ?? "tilebench.johncarmack.com",
  zoneName: process.env.TILEBENCH_ZONE_NAME ?? "johncarmack.com",
  // The parent hosted zone id, from infra/docs/.env (kept out of the repo).
  zoneId: requireEnv("TILEBENCH_ZONE_ID"),
});

function requireEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`${name} is required — set it in infra/docs/.env (see .env.example)`);
  }
  return value;
}
