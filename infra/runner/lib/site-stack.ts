import * as path from "path";
import * as cdk from "aws-cdk-lib";
import * as acm from "aws-cdk-lib/aws-certificatemanager";
import * as cloudfront from "aws-cdk-lib/aws-cloudfront";
import * as origins from "aws-cdk-lib/aws-cloudfront-origins";
import * as cognito from "aws-cdk-lib/aws-cognito";
import {
  IdentityPool,
  UserPoolAuthenticationProvider,
} from "aws-cdk-lib/aws-cognito-identitypool";
import * as ecr from "aws-cdk-lib/aws-ecr";
import * as iam from "aws-cdk-lib/aws-iam";
import * as logs from "aws-cdk-lib/aws-logs";
import * as route53 from "aws-cdk-lib/aws-route53";
import * as targets from "aws-cdk-lib/aws-route53-targets";
import * as s3 from "aws-cdk-lib/aws-s3";
import * as s3deploy from "aws-cdk-lib/aws-s3-deployment";
import * as ssm from "aws-cdk-lib/aws-ssm";
import { Construct } from "constructs";

/**
 * Fixed physical names, because `services/cloud-runner/run.sh` and `drain.sh`
 * reference them from bash without CloudFormation lookups. The bench boxes
 * write here from any region (the group lives with this stack in us-east-1).
 */
export const RUN_LOG_GROUP = "/planetiler-rs/runs";
export const BENCH_BOX_PROFILE = "planetiler-rs-bench-box";
/**
 * ECR repo the CLI runner image is pushed to (`.github/workflows/image.yml`)
 * and a dispatched bench box pulls from. Fixed name for the same reason as the
 * log group and instance profile: infra/ci grants the deploy role push access
 * by this exact ARN without a cross-stack import.
 */
export const BENCH_IMAGE_REPO = "planetiler-rs-bench";
/**
 * Bucket the job scripts ship `results/` + final logs to before a box dies
 * (`remote.sh`/`remote-planet.sh` reference it from bash, hence fixed). The
 * collection path for console-dispatched boxes, which have no ssh for scp.
 */
export const RUN_RESULTS_BUCKET = "planetiler-rs-run-results";
/**
 * The Name tag every bench box carries (run.sh and crates/run-api's
 * `cloud::BENCH_NAME` both hardcode it). RunApiStack's whole mutation
 * surface is conditioned on this tag.
 */
export const BENCH_NAME_TAG = "planetiler-bench";
/**
 * Fixed-name SSM parameters this stack publishes so RunApiStack's JWT
 * authorizer can find the pool/client without a CloudFormation export —
 * the same no-cross-stack-import convention as the names above, for values
 * that are generated rather than chosen.
 */
export const USER_POOL_ID_PARAM = "/planetiler-rs/runner/user-pool-id";
export const SPA_CLIENT_ID_PARAM = "/planetiler-rs/runner/spa-client-id";

export interface RunnerConsoleStackProps extends cdk.StackProps {
  /** Fully-qualified domain the console is served from. */
  domainName: string;
  /** Parent hosted zone the subdomain records live in. */
  zoneName: string;
  zoneId: string;
  /** Cognito hosted-UI domain prefix (globally unique within the region). */
  authDomainPrefix: string;
  /**
   * IAM Identity Center SAML metadata URL. Absent until the one-time
   * customer-managed-application wiring is done (README → Auth); when set,
   * sign-in federates to the AWS access portal and the Cognito-local owner
   * user is removed.
   */
  samlMetadataUrl?: string;
  /** Owner of the single Cognito-local user used before SAML is wired. */
  ownerEmail: string;
  /**
   * Base URL of the runner-live SSE backend (RunnerBackendStack), advertised
   * to the SPA via config.json. Unset = the live view falls back to direct
   * CloudWatch polling — the kill switch if the backend misbehaves.
   */
  liveUrl?: string;
  /**
   * Base URL of the dispatch API (RunApiStack), advertised to the SPA via
   * config.json. Unset = the console shows no dispatch controls — the kill
   * switch if the API misbehaves.
   */
  runApiUrl?: string;
}

/**
 * The runner.planetiler.rs console: a private S3 bucket served only through
 * CloudFront (OAC), a DNS-validated certificate, alias records in the parent
 * zone, and a bucket deployment that publishes `apps/runner/dist` and
 * invalidates on every `cdk deploy`. Mirrors the site stack in
 * `infra/docs`.
 *
 * Auth mirrors manifest: a Cognito user pool + hosted UI is the OIDC broker
 * (the SPA runs authorization code + PKCE in the browser), federated to IAM
 * Identity Center via SAML once the one-time app wiring exists. The SPA reads
 * its pool/client ids from a `config.json` published alongside the bundle, so
 * the build itself needs no auth context. The future run API authorizes with
 * the same pool's JWTs.
 */
export class RunnerConsoleStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: RunnerConsoleStackProps) {
    super(scope, id, props);

    const zone = route53.HostedZone.fromHostedZoneAttributes(this, "ParentZone", {
      hostedZoneId: props.zoneId,
      zoneName: props.zoneName,
    });

    const certificate = new acm.Certificate(this, "Certificate", {
      domainName: props.domainName,
      validation: acm.CertificateValidation.fromDns(zone),
    });

    // Private bucket: no public access, no website hosting — CloudFront's OAC
    // is the only reader. The site is rebuilt from the repo, so the bucket is
    // disposable with the stack.
    const bucket = new s3.Bucket(this, "SiteBucket", {
      blockPublicAccess: s3.BlockPublicAccess.BLOCK_ALL,
      enforceSSL: true,
      removalPolicy: cdk.RemovalPolicy.DESTROY,
      autoDeleteObjects: true,
    });

    const distribution = new cloudfront.Distribution(this, "Distribution", {
      comment: `${props.domainName} static site`,
      defaultBehavior: {
        origin: origins.S3BucketOrigin.withOriginAccessControl(bucket),
        viewerProtocolPolicy: cloudfront.ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
        cachePolicy: cloudfront.CachePolicy.CACHING_OPTIMIZED,
        compress: true,
      },
      defaultRootObject: "index.html",
      domainNames: [props.domainName],
      certificate,
      priceClass: cloudfront.PriceClass.PRICE_CLASS_100,
      // Single-page fallback: serve index.html for any unmatched path.
      errorResponses: [403, 404].map((httpStatus) => ({
        httpStatus,
        responseHttpStatus: 200,
        responsePagePath: "/index.html",
      })),
    });

    // -----------------------------------------------------------------
    // Auth: Cognito hosted UI (authorization code + PKCE, no client
    // secret — the SPA is the client). Before the one-time Identity
    // Center wiring, one admin-created owner signs in with a Cognito
    // password; after it, sign-in goes straight to the AWS access portal.
    // -----------------------------------------------------------------
    const pool = new cognito.UserPool(this, "Users", {
      selfSignUpEnabled: false,
      signInAliases: { email: true },
      accountRecovery: cognito.AccountRecovery.EMAIL_ONLY,
      // Holds at most one local user (none once federated) — disposable.
      removalPolicy: cdk.RemovalPolicy.DESTROY,
    });
    const poolDomain = pool.addDomain("HostedUi", {
      cognitoDomain: { domainPrefix: props.authDomainPrefix },
    });

    const SAML_PROVIDER = "IdentityCenter";
    let samlIdp: cognito.UserPoolIdentityProviderSaml | undefined;
    if (props.samlMetadataUrl) {
      samlIdp = new cognito.UserPoolIdentityProviderSaml(this, "IdentityCenter", {
        userPool: pool,
        name: SAML_PROVIDER,
        metadata: cognito.UserPoolIdentityProviderSamlMetadata.url(props.samlMetadataUrl),
        attributeMapping: { email: cognito.ProviderAttribute.other("email") },
      });
    }

    const client = pool.addClient("Spa", {
      oAuth: {
        flows: { authorizationCodeGrant: true },
        scopes: [cognito.OAuthScope.OPENID, cognito.OAuthScope.EMAIL, cognito.OAuthScope.PROFILE],
        // The localhost pair lets `pnpm dev` exercise the real sign-in
        // flow when a local config.json is present (README → Auth).
        callbackUrls: [`https://${props.domainName}/auth/callback`, "http://localhost:5173/auth/callback"],
        logoutUrls: [`https://${props.domainName}/`, "http://localhost:5173/"],
      },
      supportedIdentityProviders: samlIdp
        ? [cognito.UserPoolClientIdentityProvider.custom(SAML_PROVIDER)]
        : [cognito.UserPoolClientIdentityProvider.COGNITO],
      // No refresh flow in the SPA: a 12 h token outlives any working day,
      // and re-auth through the portal session is a silent redirect.
      idTokenValidity: cdk.Duration.hours(12),
      accessTokenValidity: cdk.Duration.hours(12),
    });
    // The client can't reference the SAML provider until it exists.
    if (samlIdp) client.node.addDependency(samlIdp);

    // Published for RunApiStack's JWT authorizer (fixed names above).
    new ssm.StringParameter(this, "UserPoolIdParam", {
      parameterName: USER_POOL_ID_PARAM,
      stringValue: pool.userPoolId,
      description: "runner console Cognito user pool id (read by RunApiStack)",
    });
    new ssm.StringParameter(this, "SpaClientIdParam", {
      parameterName: SPA_CLIENT_ID_PARAM,
      stringValue: client.userPoolClientId,
      description: "runner console SPA client id (read by RunApiStack)",
    });

    // No Cognito-local users when federated. Otherwise create the single
    // owner (Cognito emails a temp password; set a real one on first sign-in).
    if (!samlIdp) {
      new cognito.CfnUserPoolUser(this, "Owner", {
        userPoolId: pool.userPoolId,
        username: props.ownerEmail,
        userAttributes: [
          { name: "email", value: props.ownerEmail },
          { name: "email_verified", value: "true" },
        ],
      });
    }

    // -----------------------------------------------------------------
    // Live run logs: bench boxes drain job/stderr/mem files into one log
    // group (write-only, via an instance profile — no shipped
    // credentials), and the signed-in console reads them directly from
    // the browser with identity-pool credentials scoped to that group.
    // No server in between: the SPA polls GetLogEvents.
    // -----------------------------------------------------------------
    const runLogs = new logs.LogGroup(this, "RunLogs", {
      logGroupName: RUN_LOG_GROUP,
      retention: logs.RetentionDays.THREE_MONTHS,
      removalPolicy: cdk.RemovalPolicy.DESTROY,
    });
    // Both ARN forms: the bare group ARN (group-level actions) and the
    // `:*` form (stream-level actions). logGroupArn already ends in `:*`.
    const groupArns = [
      `arn:${this.partition}:logs:${this.region}:${this.account}:log-group:${RUN_LOG_GROUP}`,
      runLogs.logGroupArn,
    ];

    const boxRole = new iam.Role(this, "BenchBoxRole", {
      // Fixed like its profile (same name, different IAM namespaces), so
      // RunApiStack's Lambda can scope iam:PassRole to this exact ARN.
      roleName: BENCH_BOX_PROFILE,
      assumedBy: new iam.ServicePrincipal("ec2.amazonaws.com"),
      description: "planetiler-rs bench box: write run logs to the run log group",
      managedPolicies: [
        // Console-dispatched boxes have no ssh at all: job start/cancel is
        // SSM Run Command (crates/run-api), which needs the agent registered.
        iam.ManagedPolicy.fromAwsManagedPolicyName("AmazonSSMManagedInstanceCore"),
      ],
    });
    boxRole.addToPolicy(
      new iam.PolicyStatement({
        actions: ["logs:CreateLogStream", "logs:PutLogEvents"],
        resources: groupArns,
      }),
    );
    new iam.InstanceProfile(this, "BenchBoxProfile", {
      instanceProfileName: BENCH_BOX_PROFILE,
      role: boxRole,
    });

    // -----------------------------------------------------------------
    // Run results: the job scripts upload results/ + final logs to a
    // run-keyed prefix before the box terminates — the collection path
    // for console-dispatched boxes (no ssh, so no scp home). The bucket
    // is a transfer buffer, not the record: numbers that matter land in
    // bench/results/ in git, so objects expire with the log group's
    // retention and the bucket is disposable with the stack.
    // -----------------------------------------------------------------
    const resultsBucket = new s3.Bucket(this, "RunResults", {
      bucketName: RUN_RESULTS_BUCKET,
      blockPublicAccess: s3.BlockPublicAccess.BLOCK_ALL,
      enforceSSL: true,
      removalPolicy: cdk.RemovalPolicy.DESTROY,
      autoDeleteObjects: true,
      lifecycleRules: [{ expiration: cdk.Duration.days(90) }],
      // The signed-in console fetches result objects from the browser
      // with identity-pool credentials; same localhost pair as the auth
      // callbacks so `pnpm dev` works against real runs.
      cors: [
        {
          allowedMethods: [s3.HttpMethods.GET, s3.HttpMethods.HEAD],
          allowedOrigins: [`https://${props.domainName}`, "http://localhost:5173"],
          allowedHeaders: ["*"],
        },
      ],
    });
    boxRole.addToPolicy(
      new iam.PolicyStatement({
        actions: ["s3:PutObject"],
        resources: [resultsBucket.arnForObjects("*")],
      }),
    );

    // -----------------------------------------------------------------
    // Runner image registry: CI (image.yml) builds the release CLI and
    // pushes it here on merges to main; a console-dispatched bench box
    // pulls it instead of compiling from source. Push access lives on
    // the infra/ci deploy role (scoped to this repo's ARN by name); the
    // box's own pull access is granted here, next to its role.
    // -----------------------------------------------------------------
    const imageRepo = new ecr.Repository(this, "BenchImage", {
      repositoryName: BENCH_IMAGE_REPO,
      imageScanOnPush: true,
      // Images are rebuildable from CI, so the registry is disposable with
      // the stack. Keep only the recent tags; drop untagged layers quickly.
      removalPolicy: cdk.RemovalPolicy.DESTROY,
      emptyOnDelete: true,
      lifecycleRules: [
        {
          description: "expire untagged layers",
          tagStatus: ecr.TagStatus.UNTAGGED,
          maxImageAge: cdk.Duration.days(7),
        },
        { description: "keep the last 20 images", maxImageCount: 20 },
      ],
    });
    imageRepo.grantPull(boxRole);

    const identityPool = new IdentityPool(this, "Identity", {
      allowUnauthenticatedIdentities: false,
      authenticationProviders: {
        userPools: [new UserPoolAuthenticationProvider({ userPool: pool, userPoolClient: client })],
      },
    });
    identityPool.authenticatedRole.addToPrincipalPolicy(
      new iam.PolicyStatement({
        actions: ["logs:DescribeLogStreams", "logs:GetLogEvents", "logs:FilterLogEvents"],
        resources: groupArns,
      }),
    );
    // Read-only on the results prefix tree, so the console can list a
    // finished run's artifacts and offer downloads.
    identityPool.authenticatedRole.addToPrincipalPolicy(
      new iam.PolicyStatement({
        actions: ["s3:GetObject"],
        resources: [resultsBucket.arnForObjects("*")],
      }),
    );
    identityPool.authenticatedRole.addToPrincipalPolicy(
      new iam.PolicyStatement({
        actions: ["s3:ListBucket"],
        resources: [resultsBucket.bucketArn],
      }),
    );

    const target = route53.RecordTarget.fromAlias(
      new targets.CloudFrontTarget(distribution),
    );
    new route53.ARecord(this, "AliasA", {
      zone,
      recordName: props.domainName,
      target,
    });
    new route53.AaaaRecord(this, "AliasAaaa", {
      zone,
      recordName: props.domainName,
      target,
    });

    // Publish the built site and invalidate on deploy. `pnpm run build` must
    // have produced apps/runner/dist before `cdk deploy`. config.json rides
    // along so the SPA discovers its auth wiring at runtime.
    new s3deploy.BucketDeployment(this, "DeploySite", {
      sources: [
        s3deploy.Source.asset(path.join(__dirname, "..", "..", "..", "apps", "runner", "dist")),
        s3deploy.Source.jsonData("config.json", {
          auth: {
            domain: poolDomain.baseUrl(),
            clientId: client.userPoolClientId,
            userPoolId: pool.userPoolId,
            region: this.region,
            identityProvider: samlIdp ? SAML_PROVIDER : "",
          },
          runs: {
            region: this.region,
            logGroup: RUN_LOG_GROUP,
            identityPoolId: identityPool.identityPoolId,
            resultsBucket: RUN_RESULTS_BUCKET,
          },
          ...(props.liveUrl ? { live: { url: props.liveUrl } } : {}),
          ...(props.runApiUrl ? { runApi: { url: props.runApiUrl } } : {}),
        }),
      ],
      destinationBucket: bucket,
      distribution,
      distributionPaths: ["/*"],
    });

    new cdk.CfnOutput(this, "Url", { value: `https://${props.domainName}` });
    new cdk.CfnOutput(this, "DistributionId", { value: distribution.distributionId });
    new cdk.CfnOutput(this, "BucketName", { value: bucket.bucketName });
    new cdk.CfnOutput(this, "UserPoolId", { value: pool.userPoolId });
    new cdk.CfnOutput(this, "ClientId", { value: client.userPoolClientId });
    new cdk.CfnOutput(this, "RunLogGroup", { value: runLogs.logGroupName });
    new cdk.CfnOutput(this, "BenchBoxProfileName", { value: BENCH_BOX_PROFILE });
    new cdk.CfnOutput(this, "BenchImageRepo", {
      description: "docker pull <this>:<sha> on a dispatched bench box",
      value: imageRepo.repositoryUri,
    });
    new cdk.CfnOutput(this, "RunResultsBucket", { value: resultsBucket.bucketName });
    new cdk.CfnOutput(this, "IdentityPoolId", { value: identityPool.identityPoolId });
    // For configuring the Identity Center SAML app (README → Auth).
    new cdk.CfnOutput(this, "SamlAcsUrl", {
      description: "Identity Center SAML app: Application ACS URL",
      value: `${poolDomain.baseUrl()}/saml2/idpresponse`,
    });
    new cdk.CfnOutput(this, "SamlSpEntityId", {
      description: "Identity Center SAML app: Application SAML audience",
      value: `urn:amazon:cognito:sp:${pool.userPoolId}`,
    });
  }
}
