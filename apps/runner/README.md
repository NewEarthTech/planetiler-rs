# runner

Console for planetiler-rs cloud runs (`services/cloud-runner`): requisition a box, dispatch a job, watch the log, cancel a run, destroy the box.

This is the UI shell — no backend is wired yet. Dispatch replays the first full planet build (`bench/results/planet-run-20260722`, 2026-07-22, r7gd.8xlarge, 2 h 55 m) from a checked-in fixture at 120×, so the progress bar, stage rows, counters, and log window all exercise the exact tracing grammar the live log drain will feed later.

## Dev

- `pnpm install && pnpm dev` here, or `pnpm dev:runner` from the workspace root
- `pnpm build` / `pnpm typecheck`; lint and format come from the workspace root (`pnpm check`)

## Deploy

Live at <https://runner.johncarmack.com>. CI deploys on every push to `main` that touches the console or the shared web workspace (`.github/workflows/deploy.yml`, OIDC role from `infra/ci` — no AWS keys). `just deploy` here is the local fallback: builds `dist/` and runs the CDK deploy in `../../infra/runner` (stack `RunnerConsoleStack`, mirrors the tilebench stack in `infra/docs`); `just diff` previews. Local runs need default AWS creds and `infra/runner/.env` (see `.env.example` there).

## Roadmap

1. ~~UI shell with demo replay~~ (#48)
2. ~~Static deploy (CDK, like tilebench)~~ (this)
3. Auth (AWS SSO)
4. Batch image build/push to ECR in CI; live log drain → websocket into this UI
5. Tighten and harden
