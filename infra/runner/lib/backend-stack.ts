import * as cdk from "aws-cdk-lib";
import * as ec2 from "aws-cdk-lib/aws-ec2";
import * as iam from "aws-cdk-lib/aws-iam";
import * as route53 from "aws-cdk-lib/aws-route53";
import * as s3assets from "aws-cdk-lib/aws-s3-assets";
import { Construct } from "constructs";
import { RUN_LOG_GROUP } from "./site-stack";

/** Pinned Caddy release fetched at boot; bump deliberately. */
const CADDY_VERSION = "2.11.4";

export interface RunnerBackendStackProps extends cdk.StackProps {
  /** Fully-qualified domain the SSE backend is served from. */
  domainName: string;
  /** Parent hosted zone the subdomain record lives in. */
  zoneName: string;
  zoneId: string;
  /**
   * The runner-live release binary (aarch64-linux-gnu), produced by
   * `just runner::build-backend` locally or the backend workflow in CI.
   * Shipped as a CDK asset: its content hash lands in user-data, so a new
   * binary replaces the instance (`userDataCausesReplacement`).
   */
  binaryPath: string;
  /**
   * The console's origin: the backend reads its auth wiring from the
   * config.json published there, and allows it (plus the Vite dev server)
   * by CORS.
   */
  consoleOrigin: string;
  /** ACME account email for Caddy's certificate issuance. */
  acmeEmail: string;
}

/**
 * The consumer backend of #59: a t4g.nano running `runner-live` behind Caddy
 * on a stable domain — the vegify server pattern (tiny box, user-data pulls
 * the binary, a user-data change replaces the instance) minus the data
 * volume and Litestream, because this service is stateless: CloudWatch is
 * the durable record, the ring buffer dies with the box.
 *
 * Ubuntu 24.04 arm64, not AL2023: same OS family as the bench boxes and the
 * arm CI runners, so a binary built on either runs here without a glibc
 * mismatch. The AMI floats with Canonical's SSM parameter — a fresh AMI can
 * replace the instance on deploy, which a stateless box shrugs off.
 *
 * The instance role can read the run log group and its own binary asset,
 * and nothing else — the blast radius argued in #59. No ssh: port 22 stays
 * closed and debugging goes through SSM Session Manager.
 */
export class RunnerBackendStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: RunnerBackendStackProps) {
    super(scope, id, props);

    // Public-only single-AZ VPC: no NAT, nothing billable beyond the box and
    // its IPv4. The AZ is pinned so subnet placement never shifts between
    // deploys. (The Vpc construct still validates it via an AZ context
    // lookup; cdk.context.json is gitignored since it embeds the account id,
    // and CI re-derives it through the bootstrap lookup role.)
    const vpc = new ec2.Vpc(this, "Vpc", {
      availabilityZones: [`${this.region}a`],
      natGateways: 0,
      subnetConfiguration: [{ name: "public", subnetType: ec2.SubnetType.PUBLIC }],
    });

    const sg = new ec2.SecurityGroup(this, "Sg", {
      vpc,
      description: `${props.domainName}: Caddy in front of runner-live`,
      allowAllOutbound: true,
    });
    // 80 serves the ACME HTTP-01 fallback and the https redirect; every
    // stream request on 443 still needs a valid Cognito JWT past Caddy.
    sg.addIngressRule(ec2.Peer.anyIpv4(), ec2.Port.tcp(80), "ACME + redirect");
    sg.addIngressRule(ec2.Peer.anyIpv4(), ec2.Port.tcp(443), "SSE over TLS");

    const role = new iam.Role(this, "Role", {
      assumedBy: new iam.ServicePrincipal("ec2.amazonaws.com"),
      description: "runner-live backend: read run logs, nothing else",
      managedPolicies: [
        // Session Manager is the only way onto the box (no key pair, no 22).
        iam.ManagedPolicy.fromAwsManagedPolicyName("AmazonSSMManagedInstanceCore"),
      ],
    });
    // The log group belongs to RunnerConsoleStack; the fixed-name convention
    // means the ARNs are constructible without a cross-stack export. Live
    // Tail wants the bare group ARN; the stream-level reads want `:*`.
    const groupArn = `arn:${this.partition}:logs:${this.region}:${this.account}:log-group:${RUN_LOG_GROUP}`;
    role.addToPolicy(
      new iam.PolicyStatement({
        actions: ["logs:StartLiveTail", "logs:DescribeLogStreams", "logs:GetLogEvents"],
        resources: [groupArn, `${groupArn}:*`],
      }),
    );

    const binary = new s3assets.Asset(this, "Binary", { path: props.binaryPath });
    binary.grantRead(role);

    const userData = ec2.UserData.custom(
      [
        "#!/bin/bash",
        "set -euxo pipefail",
        // aws-cli via snap, same as drain.sh on the bench boxes.
        "snap install aws-cli --classic",
        // ---- runner-live ----
        "useradd --system --shell /usr/sbin/nologin runner",
        "mkdir -p /opt/runner-live",
        `aws s3 cp "${binary.s3ObjectUrl}" /opt/runner-live/runner-live`,
        "chmod 755 /opt/runner-live/runner-live",
        "cat > /etc/runner-live.env <<EOF",
        `RUNNER_LIVE_LOG_GROUP_ARN=${groupArn}`,
        `RUNNER_LIVE_AWS_REGION=${this.region}`,
        `RUNNER_LIVE_CONFIG_URL=${props.consoleOrigin}/config.json`,
        `RUNNER_LIVE_ALLOWED_ORIGINS=${props.consoleOrigin},http://localhost:5173`,
        "RUNNER_LIVE_LISTEN=127.0.0.1:8080",
        "EOF",
        "cat > /etc/systemd/system/runner-live.service <<'EOF'",
        "[Unit]",
        "Description=runner-live SSE backend",
        "After=network-online.target",
        "Wants=network-online.target",
        "[Service]",
        "User=runner",
        "EnvironmentFile=/etc/runner-live.env",
        "ExecStart=/opt/runner-live/runner-live",
        "Restart=always",
        "RestartSec=2",
        "[Install]",
        "WantedBy=multi-user.target",
        "EOF",
        // ---- Caddy: TLS with managed issuance on the stable domain ----
        `curl -fsSL "https://github.com/caddyserver/caddy/releases/download/v${CADDY_VERSION}/caddy_${CADDY_VERSION}_linux_arm64.tar.gz" -o /tmp/caddy.tgz`,
        "tar -xzf /tmp/caddy.tgz -C /usr/local/bin caddy",
        "useradd --system --home /var/lib/caddy --create-home --shell /usr/sbin/nologin caddy",
        "mkdir -p /etc/caddy",
        "cat > /etc/caddy/Caddyfile <<EOF",
        "{",
        `    email ${props.acmeEmail}`,
        "}",
        `${props.domainName} {`,
        "    reverse_proxy 127.0.0.1:8080",
        "}",
        "EOF",
        "cat > /etc/systemd/system/caddy.service <<'EOF'",
        "[Unit]",
        "Description=Caddy (TLS for runner-live)",
        "After=network-online.target runner-live.service",
        "Wants=network-online.target",
        "[Service]",
        "User=caddy",
        "Group=caddy",
        "ExecStart=/usr/local/bin/caddy run --config /etc/caddy/Caddyfile",
        "AmbientCapabilities=CAP_NET_BIND_SERVICE",
        "Restart=always",
        "RestartSec=2",
        "[Install]",
        "WantedBy=multi-user.target",
        "EOF",
        "systemctl daemon-reload",
        "systemctl enable --now runner-live caddy",
      ].join("\n"),
    );

    const instance = new ec2.Instance(this, "Server", {
      vpc,
      vpcSubnets: { subnetType: ec2.SubnetType.PUBLIC },
      instanceType: ec2.InstanceType.of(ec2.InstanceClass.T4G, ec2.InstanceSize.NANO),
      machineImage: ec2.MachineImage.fromSsmParameter(
        "/aws/service/canonical/ubuntu/server/24.04/stable/current/arm64/hvm/ebs-gp3/ami-id",
        { os: ec2.OperatingSystemType.LINUX },
      ),
      securityGroup: sg,
      role,
      userData,
      associatePublicIpAddress: true,
      requireImdsv2: true,
      // A binary or config change replaces the instance; boot re-runs the
      // whole user-data, so the box is always the product of one script.
      userDataCausesReplacement: true,
      blockDevices: [
        {
          deviceName: "/dev/sda1",
          volume: ec2.BlockDeviceVolume.ebs(8, { volumeType: ec2.EbsDeviceVolumeType.GP3 }),
        },
      ],
    });

    // Stable address across instance replacements: DNS points at the EIP,
    // and the association follows the new instance during a replacement.
    // Caddy retries ACME until the EIP lands on it, so first boot converges
    // without ordering ceremony.
    const eip = new ec2.CfnEIP(this, "Eip");
    new ec2.CfnEIPAssociation(this, "EipAssociation", {
      allocationId: eip.attrAllocationId,
      instanceId: instance.instanceId,
    });

    const zone = route53.HostedZone.fromHostedZoneAttributes(this, "ParentZone", {
      hostedZoneId: props.zoneId,
      zoneName: props.zoneName,
    });
    new route53.ARecord(this, "AliasA", {
      zone,
      recordName: props.domainName,
      target: route53.RecordTarget.fromIpAddresses(eip.attrPublicIp),
    });

    new cdk.CfnOutput(this, "Url", { value: `https://${props.domainName}` });
    new cdk.CfnOutput(this, "InstanceId", { value: instance.instanceId });
    new cdk.CfnOutput(this, "EipAddress", { value: eip.attrPublicIp });
  }
}
