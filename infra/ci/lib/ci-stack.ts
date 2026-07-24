import * as cdk from "aws-cdk-lib";
import * as iam from "aws-cdk-lib/aws-iam";
import { Construct } from "constructs";

// Immutable GitHub ids, rename-proof: johncarmack1984 / planetiler-rs.
const GITHUB_OWNER_ID = "20649979";
const GITHUB_REPO_ID = "1286483378";

/**
 * CI deploy role for this repo: GitHub Actions on main assumes
 * `planetiler-rs-ci-deploy` through the account's shared GitHub OIDC provider
 * (defined once in my-infra — never create a second one). The role grants
 * nothing directly except assuming the CDK bootstrap roles, which carry the
 * real deploy permissions.
 */
export class PlanetilerRsCiStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props?: cdk.StackProps) {
    super(scope, id, props);

    const role = new iam.Role(this, "DeployRole", {
      roleName: "planetiler-rs-ci-deploy",
      assumedBy: new iam.FederatedPrincipal(
        `arn:aws:iam::${this.account}:oidc-provider/token.actions.githubusercontent.com`,
        {
          StringEquals: {
            "token.actions.githubusercontent.com:aud": "sts.amazonaws.com",
          },
          // This GitHub account emits customized, id-carrying subject claims
          // (`repo:owner@ownerId/repo@repoId:ref:…`), so plain-name StringEquals
          // trust fails at AssumeRoleWithWebIdentity. Match both the id form
          // and the plain form, pinned to main.
          StringLike: {
            "token.actions.githubusercontent.com:sub": [
              `repo:*@${GITHUB_OWNER_ID}/*@${GITHUB_REPO_ID}:ref:refs/heads/main`,
              "repo:johncarmack1984/planetiler-rs:ref:refs/heads/main",
            ],
          },
        },
        "sts:AssumeRoleWithWebIdentity",
      ),
    });

    role.addToPolicy(
      new iam.PolicyStatement({
        sid: "AssumeCdkBootstrapRoles",
        actions: ["sts:AssumeRole"],
        resources: [`arn:aws:iam::${this.account}:role/cdk-hnb659fds-*`],
      }),
    );

    new cdk.CfnOutput(this, "CiDeployRoleArn", {
      value: role.roleArn,
      description: "Set this as the repo variable AWS_DEPLOY_ROLE_ARN",
    });
  }
}
