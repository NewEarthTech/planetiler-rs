# runner

Console for planetiler-rs cloud runs (`services/cloud-runner`): requisition a box, dispatch a job, watch the log, cancel a run, destroy the box.

This is the UI shell — no backend is wired yet. Dispatch replays the first full planet build (`bench/results/planet-run-20260722`, 2026-07-22, r7gd.8xlarge, 2 h 55 m) from a checked-in fixture at 120×, so the progress bar, stage rows, counters, and log window all exercise the exact tracing grammar the live log drain will feed later.

## Dev

- `pnpm install && pnpm dev` here, or `pnpm dev:runner` from the workspace root
- `pnpm build` / `pnpm typecheck`; lint and format come from the workspace root (`pnpm check`)

## Roadmap

1. UI shell with demo replay (this)
2. Static deploy (CDK, like tilebench)
3. Auth (AWS SSO)
4. Batch image build/push to ECR in CI; live log drain → websocket into this UI
5. Tighten and harden
