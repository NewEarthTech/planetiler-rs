# ci-infra

The GitHub Actions → AWS bridge for this repo: one IAM role (`planetiler-rs-ci-deploy`) that `.github/workflows/deploy.yml` assumes via the account's shared GitHub OIDC provider, trust pinned to this repo's `main`. The role itself can only assume the CDK bootstrap roles — the actual deploy permissions live there.

One-time wiring, run locally with admin creds: `just ci::deploy`, then set the `CiDeployRoleArn` output as the repo variable `AWS_DEPLOY_ROLE_ARN` (plus `RUNNER_ZONE_ID` for the runner stack). No `.env` — the stack has no configuration.
