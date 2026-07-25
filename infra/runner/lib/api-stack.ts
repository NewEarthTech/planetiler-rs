import * as cdk from "aws-cdk-lib";
import * as apigwv2 from "aws-cdk-lib/aws-apigatewayv2";
import { HttpJwtAuthorizer } from "aws-cdk-lib/aws-apigatewayv2-authorizers";
import { HttpLambdaIntegration } from "aws-cdk-lib/aws-apigatewayv2-integrations";
import * as acm from "aws-cdk-lib/aws-certificatemanager";
import * as iam from "aws-cdk-lib/aws-iam";
import * as lambda from "aws-cdk-lib/aws-lambda";
import * as logs from "aws-cdk-lib/aws-logs";
import * as route53 from "aws-cdk-lib/aws-route53";
import * as targets from "aws-cdk-lib/aws-route53-targets";
import * as ssm from "aws-cdk-lib/aws-ssm";
import { Construct } from "constructs";
import {
  BENCH_BOX_PROFILE,
  BENCH_IMAGE_REPO,
  BENCH_NAME_TAG,
  SPA_CLIENT_ID_PARAM,
  USER_POOL_ID_PARAM,
} from "./site-stack";

export interface RunApiStackProps extends cdk.StackProps {
  /** Fully-qualified domain the API is served from. */
  domainName: string;
  /** Parent hosted zone the subdomain record lives in. */
  zoneName: string;
  zoneId: string;
  /** Origin the console calls from — every request is cross-origin CORS. */
  consoleOrigin: string;
  /** runner-live base URL, for the events links the API hands out. */
  liveBase: string;
  /**
   * Directory holding the `bootstrap` Lambda binary (aarch64 musl static),
   * produced by `just runner::build-api` locally or the api workflow in CI.
   */
  distDir: string;
}

/**
 * The dispatch API of #59: `crates/run-api` on an arm64 Lambda behind an API
 * Gateway HTTP API. The control plane's deliberate shape — scary permissions
 * on a process that exists for milliseconds behind a JWT authorizer, not on
 * the internet-facing nano. The authorizer verifies the console's Cognito id
 * tokens, so the id token the SPA already holds is the API credential and no
 * new auth surface exists.
 *
 * The pool/client ids come from the fixed-name SSM parameters
 * RunnerConsoleStack publishes — resolved at deploy time, so there is no
 * CloudFormation export lock-in and this stack deploys without the console
 * stack present in the same synth (#64's guarded-synth pattern).
 *
 * Every mutating EC2/SSM permission is tag-conditioned on the bench Name
 * tag, so this API can never touch anything but bench boxes; the launch
 * statement requires that same tag on the instance it creates.
 */
export class RunApiStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: RunApiStackProps) {
    super(scope, id, props);

    const zone = route53.HostedZone.fromHostedZoneAttributes(this, "ParentZone", {
      hostedZoneId: props.zoneId,
      zoneName: props.zoneName,
    });

    const poolId = ssm.StringParameter.valueForStringParameter(this, USER_POOL_ID_PARAM);
    const clientId = ssm.StringParameter.valueForStringParameter(this, SPA_CLIENT_ID_PARAM);
    const authorizer = new HttpJwtAuthorizer(
      "Jwt",
      `https://cognito-idp.${this.region}.amazonaws.com/${poolId}`,
      { jwtAudience: [clientId] },
    );

    const handler = new lambda.Function(this, "Handler", {
      description: "planetiler-rs run-api: requisition/dispatch/cancel/destroy/list bench boxes",
      runtime: lambda.Runtime.PROVIDED_AL2023,
      architecture: lambda.Architecture.ARM_64,
      handler: "bootstrap",
      code: lambda.Code.fromAsset(props.distDir),
      memorySize: 256,
      timeout: cdk.Duration.seconds(30),
      environment: {
        RUN_API_IMAGE_REPO: `${this.account}.dkr.ecr.${this.region}.amazonaws.com/${BENCH_IMAGE_REPO}`,
        RUN_API_LIVE_BASE: props.liveBase,
      },
      logGroup: new logs.LogGroup(this, "HandlerLogs", {
        retention: logs.RetentionDays.ONE_MONTH,
        removalPolicy: cdk.RemovalPolicy.DESTROY,
      }),
    });

    // ---- The blast radius, itemized (#59's security posture) ----
    // Launch: the instance being created must carry the bench Name tag...
    handler.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["ec2:RunInstances"],
        resources: [`arn:${this.partition}:ec2:*:${this.account}:instance/*`],
        conditions: { StringEquals: { "aws:RequestTag/Name": BENCH_NAME_TAG } },
      }),
    );
    // ...and the untagged collaterals of a launch (per-resource evaluation
    // means the tag condition above cannot cover these).
    handler.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["ec2:RunInstances"],
        resources: [
          `arn:${this.partition}:ec2:*::image/*`,
          `arn:${this.partition}:ec2:*:${this.account}:network-interface/*`,
          `arn:${this.partition}:ec2:*:${this.account}:volume/*`,
          `arn:${this.partition}:ec2:*:${this.account}:security-group/*`,
          `arn:${this.partition}:ec2:*:${this.account}:subnet/*`,
        ],
      }),
    );
    // Tag-on-create for the launch's TagSpecification, and the run-id tag
    // dispatch writes later — the latter only on boxes already wearing the
    // bench tag.
    handler.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["ec2:CreateTags"],
        resources: [`arn:${this.partition}:ec2:*:${this.account}:instance/*`],
        conditions: { StringEquals: { "ec2:CreateAction": "RunInstances" } },
      }),
    );
    handler.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["ec2:CreateTags"],
        resources: [`arn:${this.partition}:ec2:*:${this.account}:instance/*`],
        conditions: { StringEquals: { "ec2:ResourceTag/Name": BENCH_NAME_TAG } },
      }),
    );
    handler.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["ec2:TerminateInstances"],
        resources: [`arn:${this.partition}:ec2:*:${this.account}:instance/*`],
        conditions: { StringEquals: { "ec2:ResourceTag/Name": BENCH_NAME_TAG } },
      }),
    );
    // Describe has no resource-level scoping; the routes filter by tag.
    handler.addToRolePolicy(
      new iam.PolicyStatement({ actions: ["ec2:DescribeInstances"], resources: ["*"] }),
    );
    // Hand the bench-box role (fixed name, shared with its profile) to the
    // instances this API launches — and to nothing else, for nothing else.
    handler.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["iam:PassRole"],
        resources: [`arn:${this.partition}:iam::${this.account}:role/${BENCH_BOX_PROFILE}`],
        conditions: { StringEquals: { "iam:PassedToService": "ec2.amazonaws.com" } },
      }),
    );
    // The canonical Ubuntu AMI parameter (public, both box regions).
    handler.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["ssm:GetParameter"],
        resources: [`arn:${this.partition}:ssm:*::parameter/aws/service/canonical/ubuntu/*`],
      }),
    );
    // SendCommand authorizes the document and the instance separately; only
    // bench-tagged instances are reachable.
    handler.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["ssm:SendCommand"],
        resources: [`arn:${this.partition}:ssm:*::document/AWS-RunShellScript`],
      }),
    );
    handler.addToRolePolicy(
      new iam.PolicyStatement({
        actions: ["ssm:SendCommand"],
        resources: [`arn:${this.partition}:ec2:*:${this.account}:instance/*`],
        conditions: { StringEquals: { "ssm:resourceTag/Name": BENCH_NAME_TAG } },
      }),
    );
    handler.addToRolePolicy(
      new iam.PolicyStatement({ actions: ["ssm:GetCommandInvocation"], resources: ["*"] }),
    );

    const certificate = new acm.Certificate(this, "Certificate", {
      domainName: props.domainName,
      validation: acm.CertificateValidation.fromDns(zone),
    });
    const domain = new apigwv2.DomainName(this, "Domain", {
      domainName: props.domainName,
      certificate,
    });

    const api = new apigwv2.HttpApi(this, "Api", {
      description: `${props.domainName}: the runner console's dispatch API`,
      // Every browser request is cross-origin (runner. vs api.runner.) and
      // carries Authorization, so it's all preflighted; the gateway answers
      // OPTIONS itself, before the authorizer.
      corsPreflight: {
        allowOrigins: [props.consoleOrigin, "http://localhost:5173"],
        allowMethods: [
          apigwv2.CorsHttpMethod.GET,
          apigwv2.CorsHttpMethod.POST,
          apigwv2.CorsHttpMethod.DELETE,
        ],
        allowHeaders: ["authorization", "content-type"],
        maxAge: cdk.Duration.hours(1),
      },
      defaultAuthorizer: authorizer,
      defaultDomainMapping: { domainName: domain },
      // The stable name is the only door; no execute-api URL to leak.
      disableExecuteApiEndpoint: true,
    });
    // Explicit methods, not ANY: an ANY route also matches OPTIONS, which
    // sends browser preflights to the JWT authorizer — and preflights never
    // carry Authorization, so every console call would 401 at the gate.
    // With no route matching OPTIONS, the gateway's automatic CORS responder
    // answers preflights itself (verified: ANY gave 401, this gives 204).
    api.addRoutes({
      path: "/{proxy+}",
      methods: [apigwv2.HttpMethod.GET, apigwv2.HttpMethod.POST, apigwv2.HttpMethod.DELETE],
      integration: new HttpLambdaIntegration("Lambda", handler),
    });

    const target = route53.RecordTarget.fromAlias(
      new targets.ApiGatewayv2DomainProperties(
        domain.regionalDomainName,
        domain.regionalHostedZoneId,
      ),
    );
    new route53.ARecord(this, "AliasA", { zone, recordName: props.domainName, target });
    new route53.AaaaRecord(this, "AliasAaaa", { zone, recordName: props.domainName, target });

    new cdk.CfnOutput(this, "Url", { value: `https://${props.domainName}` });
  }
}
