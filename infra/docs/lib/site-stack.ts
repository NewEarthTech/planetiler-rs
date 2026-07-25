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

export interface TilebenchStackProps extends cdk.StackProps {
  /** Fully-qualified domain the site is served from (the apex). */
  domainName: string;
  /** Hosted zone the records live in. */
  zoneName: string;
  zoneId: string;
}

/**
 * The planetiler.rs site: a private S3 bucket served only through CloudFront
 * (OAC), a DNS-validated certificate, alias records in the zone, and two
 * bucket deployments — the splash placeholder at the root and the tilebench
 * dashboard (`apps/docs/dist`, built with `base: "/benchmarks/"`) under
 * `/benchmarks` — with an invalidation on every `cdk deploy`.
 */
export class TilebenchStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: TilebenchStackProps) {
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

    // S3 has no directory indexes and defaultRootObject only covers "/", so
    // "/benchmarks" and "/benchmarks/" would otherwise miss the bucket and
    // fall through to the splash via the 403 handler below.
    const indexRewrite = new cloudfront.Function(this, "IndexRewrite", {
      comment: "route /benchmarks to the dashboard's index.html",
      code: cloudfront.FunctionCode.fromInline(
        [
          "function handler(event) {",
          "  var request = event.request;",
          "  var uri = request.uri;",
          "  if (uri === '/benchmarks' || uri === '/benchmarks/') {",
          "    request.uri = '/benchmarks/index.html';",
          "  } else if (uri.indexOf('/benchmarks/') === 0 && uri.split('/').pop().indexOf('.') === -1) {",
          "    request.uri = '/benchmarks/index.html';",
          "  }",
          "  return request;",
          "}",
        ].join("\n"),
      ),
    });

    const distribution = new cloudfront.Distribution(this, "Distribution", {
      comment: `${props.domainName} static site`,
      defaultBehavior: {
        origin: origins.S3BucketOrigin.withOriginAccessControl(bucket),
        viewerProtocolPolicy: cloudfront.ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
        cachePolicy: cloudfront.CachePolicy.CACHING_OPTIMIZED,
        compress: true,
        functionAssociations: [
          {
            function: indexRewrite,
            eventType: cloudfront.FunctionEventType.VIEWER_REQUEST,
          },
        ],
      },
      defaultRootObject: "index.html",
      domainNames: [props.domainName],
      certificate,
      priceClass: cloudfront.PriceClass.PRICE_CLASS_100,
      // Any path that misses the bucket serves the splash page.
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

    // The splash placeholder at the root. prune stays off so this deployment
    // never sweeps the dashboard out from under /benchmarks.
    new s3deploy.BucketDeployment(this, "DeploySplash", {
      sources: [s3deploy.Source.asset(path.join(__dirname, "..", "..", "..", "apps", "splash"))],
      destinationBucket: bucket,
      prune: false,
      distribution,
      distributionPaths: ["/*"],
    });

    // The dashboard under /benchmarks. `pnpm run build` must have produced
    // apps/docs/dist before `cdk deploy`; prune is scoped to the prefix, so
    // stale hashed assets are cleaned without touching the splash.
    new s3deploy.BucketDeployment(this, "DeploySite", {
      sources: [s3deploy.Source.asset(path.join(__dirname, "..", "..", "..", "apps", "docs", "dist"))],
      destinationBucket: bucket,
      destinationKeyPrefix: "benchmarks/",
      distribution,
      distributionPaths: ["/benchmarks/*"],
    });

    new cdk.CfnOutput(this, "Url", { value: `https://${props.domainName}` });
    new cdk.CfnOutput(this, "BenchmarksUrl", { value: `https://${props.domainName}/benchmarks` });
    new cdk.CfnOutput(this, "DistributionId", { value: distribution.distributionId });
    new cdk.CfnOutput(this, "BucketName", { value: bucket.bucketName });
  }
}
