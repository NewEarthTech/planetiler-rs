#!/usr/bin/env node
import "source-map-support/register";
import * as cdk from "aws-cdk-lib";
import { PlanetilerRsZoneStack } from "../lib/zone-stack";

const app = new cdk.App();

// Whatever account your AWS credentials point at. No account id is baked in.
const account = process.env.CDK_DEFAULT_ACCOUNT;

cdk.Tags.of(app).add("Project", "planetiler-rs");
cdk.Tags.of(app).add("ManagedBy", "cdk");

new PlanetilerRsZoneStack(app, "PlanetilerRsZoneStack", {
  // Route 53 is global; us-east-1 keeps the stack beside the rest of the estate.
  env: { account, region: "us-east-1" },
  zoneName: "planetiler.rs",
});
