# runner

Console for planetiler-rs cloud runs (`services/cloud-runner`): watch a dispatched box's log live, or replay the recorded planet run. Dispatch itself still happens from the laptop.

Two sources, one UI: the **demo replay** streams the first full planet build (`bench/results/planet-run-20260722`, 2026-07-22, r7gd.8xlarge, 2 h 55 m) from a checked-in fixture at 120×; **live runs** tail the box's actual log out of CloudWatch Logs through the same parser, progress model, and log window.

## Live runs

Write side: `run.sh` attaches the `planetiler-rs-bench-box` instance profile (created by this stack) when it exists, and starts `drain.sh` on the box beside the job — it tails `job.log`, `planet-rs.stderr`, and `planet-mem.log` into streams `<run-id>/{job,planetiler,mem}` of the `/planetiler-rs/runs` log group every ~1 s. Read side: when `config.json` advertises the SSE backend (see Live backend below), the signed-in SPA holds one streaming response per run — `src/lib/live.ts`, fetch + `ReadableStream` with the id token in `Authorization` — and lines land as they're ingested, the mem readout riding the same stream; without the advert it falls back to the original identity-pool path, polling `GetLogEvents` every 3 s. The run picker lists what the log group knows (still the identity-pool read); a planet run shows live phase progress, counters, and the mem-sampler's RSS/anon/load readout.

Everything is additive: a box launched without the instance profile (stack not deployed, or a fresh account) benches exactly as before — the drain gives up quietly and the laptop-side collectors (`run.sh` polling, `watch.sh`) remain the collectors of record.

## Live backend (#59)

The next tier of the live path: `crates/runner-live` (#60) is a consumer backend that holds a CloudWatch **Live Tail** session over the run log group — a streaming call, opened only while at least one client is attached — parses lines into typed events, ring-buffers per-run history, and serves it all as **SSE** with `Last-Event-ID` resume (`GET /runs/{run-id}/events`: history first, then live, one response — the Vercel `?follow=1` shape). Auth is the same Cognito id token the SPA already holds, verified against the pool JWKS from the site's published `config.json`; the ingest transport sits behind a `LogSource` seam so a direct box→backend stream can swap in later without touching the parser, buffer, or client leg.

It runs on `RunnerBackendStack` (`infra/runner/lib/backend-stack.ts`): a t4g.nano (Ubuntu 24.04 arm64 — glibc-matched to the arm CI runners that build the binary) behind Caddy, which owns TLS on `live.runner.planetiler.rs`; an EIP keeps the address stable across instance replacements. The binary ships as a CDK asset whose hash rides in user-data, so `userDataCausesReplacement` turns every new binary into a fresh instance built by one boot script. The instance role reads the run log group and its own asset, and nothing else; there is no ssh — port 22 is closed and debugging goes through SSM Session Manager (`aws ssm start-session --target <InstanceId>`). The SPA consumes the stream when `config.json` carries a `live.url` (the stack publishes it from `RUNNER_LIVE_DOMAIN`; set that variable empty to kill-switch back to CloudWatch polling), and the backend allows the console's origin by CORS — the Authorization header makes every browser call preflighted.

## Backend deploy

First deploy is deliberately by hand (it creates the EIP and DNS record): `just runner::deploy-backend` — cross-builds `runner-live` for aarch64-linux in Docker (`rust:1-bookworm`, cached in named volumes), then deploys the stack; `just runner::diff-backend` previews. After that, set the `BACKEND_DEPLOY_ENABLED=true` repo variable and `.github/workflows/backend.yml` redeploys on every push to main touching the crate or the stack, building natively on an arm64 runner with the same OIDC role as deploy.yml.

## Dev

- `pnpm install && pnpm dev` here, or `pnpm dev:runner` from the workspace root
- `pnpm build` / `pnpm typecheck`; lint and format come from the workspace root (`pnpm check`)

## Auth

The deployed console sits behind a sign-in gate (mirrors manifest): a Cognito user pool + hosted UI is the OIDC broker, the SPA runs authorization code + PKCE in the browser (`src/lib/auth.ts`, no SDK), and the future run API will authorize with the same pool's JWTs. The SPA reads its pool/client ids at boot from `/config.json`, which the CDK stack publishes next to the bundle — the build itself needs no auth context, and a production deploy with no config fails closed.

Out of the box sign-in uses one admin-created Cognito user (`RUNNER_OWNER_EMAIL`; Cognito emails a temporary password on first deploy). To sign in with AWS IAM Identity Center instead — no separate password — federate Cognito to Identity Center via SAML:

1. Deploy once *without* `RUNNER_SAML_METADATA_URL`. Note the stack outputs `SamlAcsUrl` and `SamlSpEntityId`.
2. In IAM Identity Center → Applications → **Add customer managed application** (SAML 2.0): set **Application ACS URL** = `SamlAcsUrl`, **Application SAML audience** = `SamlSpEntityId`, add an attribute mapping `email` → `${user:email}` (Subject/NameID = email), and assign yourself. Copy the app's **IAM Identity Center SAML metadata** URL.
3. Set the `RUNNER_SAML_METADATA_URL` repo variable in GitHub (CI deploys pass it through; for local deploys set it in `infra/runner/.env` too) and deploy again. Sign-in now goes straight to the AWS access portal and the Cognito-local user is removed.

Plain `pnpm dev` runs unauthenticated — there is no `/config.json`, and the demo needs no AWS. To exercise the real sign-in flow locally, copy the deployed config (`curl https://runner.planetiler.rs/config.json`) to `public/config.json` (gitignored); `http://localhost:5173/auth/callback` is registered on the pool client for exactly this.

## Deploy

Live at <https://runner.planetiler.rs>. CI deploys on every push to `main` that touches the console or the shared web workspace (`.github/workflows/deploy.yml`, OIDC role from `infra/ci` — no AWS keys). Local fallback: `just runner::deploy` from anywhere in the repo (stack `RunnerConsoleStack`, mirrors the tilebench stack in `infra/docs`); `just runner::diff` previews. Local runs need default AWS creds and `infra/runner/.env` (see `.env.example` there).

## Roadmap

1. ~~UI shell with demo replay~~ (#48)
2. ~~Static deploy (CDK, like tilebench)~~ (#49, CI-deployed since #50)
3. ~~Auth (AWS SSO)~~ (#55)
4. ~~Live log drain into this UI~~ (#56 — CloudWatch Logs + identity-pool polling, not a websocket)
5. ~~Runner image build/push to ECR in CI~~ (this — see Runner image below)
6. Console-side dispatch (#59), in slices: ~~the `runner-live` SSE backend~~ (#60), ~~its stack~~ (#61, live at live.runner.planetiler.rs), ~~the SPA's switch to the SSE stream~~ (this); next the run API + ActionBar wiring
7. Tighten and harden

## Runner image

`.github/workflows/image.yml` builds the release CLI (`services/cloud-runner/Dockerfile`, a minimal glibc image with `planetiler-rs` as the entrypoint) natively on arm64 and, on merges to main, pushes `planetiler-rs-bench:<sha>` and `:main` to ECR. The repository is created by `RunnerConsoleStack` next to the bench-box instance profile, which can already pull it; push access lives on the infra/ci deploy role (no AWS keys — OIDC). A console-dispatched box will `docker pull` this instead of compiling from source the way `services/cloud-runner/remote*.sh` do today — that dispatch path is the next phase, not this one.
