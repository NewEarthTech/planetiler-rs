import * as path from "path";
import * as cdk from "aws-cdk-lib";
import * as acm from "aws-cdk-lib/aws-certificatemanager";
import * as cloudfront from "aws-cdk-lib/aws-cloudfront";
import * as origins from "aws-cdk-lib/aws-cloudfront-origins";
import * as cognito from "aws-cdk-lib/aws-cognito";
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
}

/**
 * The runner.johncarmack.com console: a private S3 bucket served only through
 * CloudFront (OAC), a DNS-validated certificate, alias records in the parent
 * zone, and a bucket deployment that publishes `apps/runner/dist` and
 * invalidates on every `cdk deploy`. Mirrors the tilebench stack in
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
