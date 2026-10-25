import * as cdk from "aws-cdk-lib";
import * as iam from "aws-cdk-lib/aws-iam";
import { Construct } from "constructs";

// GitHub ids. The repo id (1286483378) is stable across renames AND transfers;
// the OWNER id is NOT — it changes when a repo moves between accounts. It did on
// the 2026-07-24 transfer from the johncarmack1984 user (20649979) to the
// NewEarthTech org (147949794), which broke the OIDC trust (every deploy failed
// at AssumeRoleWithWebIdentity) until this owner id and the plain-name claim
// below were updated. The subject below names the owner and repo too, so after
// any future rename or transfer update it and re-run `just ci::deploy`.
const GITHUB_OWNER_ID = "147949794"; // NewEarthTech org
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
          // The subject GitHub's tokens carry for this repo's main, matched
          // whole: the immutable form GitHub reports for this repo
          // (`repo:owner@ownerId/repo@repoId:ref:…`), and the plain-name form
          // in case the repo goes back to the default claim. No wildcard: IAM's
          // `*` also matches `:`, `/` and `@`, and any repository can choose
          // subject claims that carry these ids, so the earlier
          // `repo:*@ownerId/*@repoId` form let other repositories' tokens match.
          StringEquals: {
            "token.actions.githubusercontent.com:aud": "sts.amazonaws.com",
            "token.actions.githubusercontent.com:sub": [
              `repo:NewEarthTech@${GITHUB_OWNER_ID}/planetiler-rs@${GITHUB_REPO_ID}:ref:refs/heads/main`,
              "repo:NewEarthTech/planetiler-rs:ref:refs/heads/main",
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

    // Push the runner image to ECR (.github/workflows/image.yml). Unlike a
    // `cdk deploy`, `docker push` isn't brokered by the bootstrap roles, so the
    // deploy role needs ECR access directly. The repository is created in
    // RunnerConsoleStack (infra/runner) with the fixed name below and lives in
    // us-east-1 with the rest of the runner infra; referencing it by ARN string
    // (not a cross-stack import) keeps the two islands independently deployable.
    role.addToPolicy(
      new iam.PolicyStatement({
        sid: "EcrLogin",
        // Account-level action — cannot be scoped to a single repository.
        actions: ["ecr:GetAuthorizationToken"],
        resources: ["*"],
      }),
    );
    role.addToPolicy(
      new iam.PolicyStatement({
        sid: "PushRunnerImage",
        actions: [
          "ecr:BatchCheckLayerAvailability",
          "ecr:InitiateLayerUpload",
          "ecr:UploadLayerPart",
          "ecr:CompleteLayerUpload",
          "ecr:PutImage",
          // Read existing layers/tags so buildx cache-from and re-tagging work.
          "ecr:BatchGetImage",
          "ecr:GetDownloadUrlForLayer",
        ],
        resources: [`arn:${this.partition}:ecr:us-east-1:${this.account}:repository/planetiler-rs-bench`],
      }),
    );

    new cdk.CfnOutput(this, "CiDeployRoleArn", {
      value: role.roleArn,
      description: "Set this as the repo variable AWS_DEPLOY_ROLE_ARN",
    });
  }
}
