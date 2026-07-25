import * as cdk from "aws-cdk-lib";
import * as route53 from "aws-cdk-lib/aws-route53";
import { Construct } from "constructs";

export interface PlanetilerRsZoneStackProps extends cdk.StackProps {
  /** Apex domain the hosted zone is authoritative for. */
  zoneName: string;
}

/**
 * The Route 53 public hosted zone for planetiler.rs. The registrar (Regery)
 * delegates to the four name servers this stack outputs, and that delegation
 * is pasted in by hand — so the zone is retained on stack deletion, because a
 * recreated zone gets a fresh NS set and the delegation would silently break.
 */
export class PlanetilerRsZoneStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: PlanetilerRsZoneStackProps) {
    super(scope, id, props);

    const zone = new route53.PublicHostedZone(this, "Zone", {
      zoneName: props.zoneName,
    });
    zone.applyRemovalPolicy(cdk.RemovalPolicy.RETAIN);

    new cdk.CfnOutput(this, "ZoneId", { value: zone.hostedZoneId });
    new cdk.CfnOutput(this, "NameServers", {
      value: cdk.Fn.join(" ", zone.hostedZoneNameServers ?? []),
    });
  }
}
