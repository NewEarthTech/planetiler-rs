import * as path from "path";
import * as cdk from "aws-cdk-lib";
import * as acm from "aws-cdk-lib/aws-certificatemanager";
import * as cloudfront from "aws-cdk-lib/aws-cloudfront";
import * as origins from "aws-cdk-lib/aws-cloudfront-origins";
import * as route53 from "aws-cdk-lib/aws-route53";
import * as targets from "aws-cdk-lib/aws-route53-targets";
import * as s3 from "aws-cdk-lib/aws-s3";
import * as s3deploy from "aws-cdk-lib/aws-s3-deployment";
import { Construct } from "constructs";

export interface RunnerConsoleStackProps extends cdk.StackProps {
  /** Fully-qualified domain the console is served from. */
  domainName: string;
  /** Parent hosted zone the subdomain records live in. */
  zoneName: string;
  zoneId: string;
}

/**
 * The runner.johncarmack.com console: a private S3 bucket served only through
 * CloudFront (OAC), a DNS-validated certificate, alias records in the parent
 * zone, and a bucket deployment that publishes `apps/runner/dist` and
 * invalidates on every `cdk deploy`. Mirrors the tilebench stack in
 * `infra/docs`; today it ships the static demo shell — the live backend
 * (auth, log drain) is a later roadmap step.
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
    // have produced apps/runner/dist before `cdk deploy`.
    new s3deploy.BucketDeployment(this, "DeploySite", {
      sources: [s3deploy.Source.asset(path.join(__dirname, "..", "..", "..", "apps", "runner", "dist"))],
      destinationBucket: bucket,
      distribution,
      distributionPaths: ["/*"],
    });

    new cdk.CfnOutput(this, "Url", { value: `https://${props.domainName}` });
    new cdk.CfnOutput(this, "DistributionId", { value: distribution.distributionId });
    new cdk.CfnOutput(this, "BucketName", { value: bucket.bucketName });
  }
}
