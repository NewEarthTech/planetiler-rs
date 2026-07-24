# runner

Console for planetiler-rs cloud runs (`services/cloud-runner`): watch a dispatched box's log live, or replay the recorded planet run. Dispatch itself still happens from the laptop.

Two sources, one UI: the **demo replay** streams the first full planet build (`bench/results/planet-run-20260722`, 2026-07-22, r7gd.8xlarge, 2 h 55 m) from a checked-in fixture at 120×; **live runs** tail the box's actual log out of CloudWatch Logs through the same parser, progress model, and log window.

## Live runs

The drain is deliberately serverless on both ends. Write side: `run.sh` attaches the `planetiler-rs-bench-box` instance profile (created by this stack) when it exists, and starts `drain.sh` on the box beside the job — it tails `job.log`, `planet-rs.stderr`, and `planet-mem.log` into streams `<run-id>/{job,planetiler,mem}` of the `/planetiler-rs/runs` log group every ~5 s. Read side: the signed-in SPA trades its Cognito id token for identity-pool credentials scoped to read that one log group and polls `GetLogEvents` every 3 s — livelier than the 80 s scp loop in `watch.sh`, with no Lambda, API Gateway, or websocket state to operate (the roadmap said "websocket"; polling a managed store won on moving parts). The run picker lists what the log group knows; a planet run shows live phase progress, counters, and the mem-sampler's RSS/anon/load readout.

Everything is additive: a box launched without the instance profile (stack not deployed, or a fresh account) benches exactly as before — the drain gives up quietly and the laptop-side collectors (`run.sh` polling, `watch.sh`) remain the collectors of record.

## Dev

- `pnpm install && pnpm dev` here, or `pnpm dev:runner` from the workspace root
- `pnpm build` / `pnpm typecheck`; lint and format come from the workspace root (`pnpm check`)

## Auth

The deployed console sits behind a sign-in gate (mirrors manifest): a Cognito user pool + hosted UI is the OIDC broker, the SPA runs authorization code + PKCE in the browser (`src/lib/auth.ts`, no SDK), and the future run API will authorize with the same pool's JWTs. The SPA reads its pool/client ids at boot from `/config.json`, which the CDK stack publishes next to the bundle — the build itself needs no auth context, and a production deploy with no config fails closed.

Out of the box sign-in uses one admin-created Cognito user (`RUNNER_OWNER_EMAIL`; Cognito emails a temporary password on first deploy). To sign in with AWS IAM Identity Center instead — no separate password — federate Cognito to Identity Center via SAML:

1. Deploy once *without* `RUNNER_SAML_METADATA_URL`. Note the stack outputs `SamlAcsUrl` and `SamlSpEntityId`.
2. In IAM Identity Center → Applications → **Add customer managed application** (SAML 2.0): set **Application ACS URL** = `SamlAcsUrl`, **Application SAML audience** = `SamlSpEntityId`, add an attribute mapping `email` → `${user:email}` (Subject/NameID = email), and assign yourself. Copy the app's **IAM Identity Center SAML metadata** URL.
3. Set the `RUNNER_SAML_METADATA_URL` repo variable in GitHub (CI deploys pass it through; for local deploys set it in `infra/runner/.env` too) and deploy again. Sign-in now goes straight to the AWS access portal and the Cognito-local user is removed.

Plain `pnpm dev` runs unauthenticated — there is no `/config.json`, and the demo needs no AWS. To exercise the real sign-in flow locally, copy the deployed config (`curl https://runner.johncarmack.com/config.json`) to `public/config.json` (gitignored); `http://localhost:5173/auth/callback` is registered on the pool client for exactly this.

## Deploy

Live at <https://runner.johncarmack.com>. CI deploys on every push to `main` that touches the console or the shared web workspace (`.github/workflows/deploy.yml`, OIDC role from `infra/ci` — no AWS keys). Local fallback: `just runner::deploy` from anywhere in the repo (stack `RunnerConsoleStack`, mirrors the tilebench stack in `infra/docs`); `just runner::diff` previews. Local runs need default AWS creds and `infra/runner/.env` (see `.env.example` there).

## Roadmap

1. ~~UI shell with demo replay~~ (#48)
2. ~~Static deploy (CDK, like tilebench)~~ (#49, CI-deployed since #50)
3. ~~Auth (AWS SSO)~~ (#55)
4. ~~Live log drain into this UI~~ (this — CloudWatch Logs + identity-pool polling, not a websocket)
5. Runner image build/push to ECR in CI (toward console-side dispatch)
6. Tighten and harden
