#!/usr/bin/env node
import "source-map-support/register";
import * as cdk from "aws-cdk-lib";
import { PlanetilerRsCiStack } from "../lib/ci-stack";

const app = new cdk.App();

// Whatever account your AWS credentials point at. No account id is baked in.
const account = process.env.CDK_DEFAULT_ACCOUNT;

cdk.Tags.of(app).add("Project", "planetiler-rs");
cdk.Tags.of(app).add("ManagedBy", "cdk");

new PlanetilerRsCiStack(app, "PlanetilerRsCiStack", {
  // Alongside the site stacks this role deploys.
  env: { account, region: "us-east-1" },
});
