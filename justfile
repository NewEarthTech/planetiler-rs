# Entry point for the estate's justfiles: each infra island is a module, its
# recipes run in its own directory. `just --list` shows the tree;
# e.g. `just runner::deploy`, `just docs::diff`, `just ci::deploy`.
#
# The recipes here are the ones that aren't infrastructure: making a run, and
# watching one.

set shell := ["bash", "-cu"]

mod ci "infra/ci"
mod dns "infra/dns"
mod docs "infra/docs"
mod runner "infra/runner"

# Show the recipe tree.
default:
    @just --list

# The job (services/cloud-runner/jobs.sh) fixes the input and the render flags;
# where you are decides the rest. On a laptop that means files under
# ~/.planetiler-rs and no AWS at all; on a bench box, the instance-store
# scratch, the memory watchdog, the CloudWatch drain, and results to S3 —
# run-job.sh detects which, so the command is the same on both sides and the
# two runs compare 1:1 on input.
#
#   just run                      # monaco, the smoke job
#   just run rhode-island         # 49 MB, still an interactive command
#   just run planet               # if you have the disk and the day
#   just run monaco --save-s3     # local run, results shipped to S3 after
#
# Watch it with `just ui` — before, during, or after; the run doesn't care.

# Make a run: local on a laptop, cloud on a bench box, same job spec either way.
run job='monaco' *args='':
    services/cloud-runner/run-job.sh {{ job }} {{ args }}

# The SSE backend over ~/.planetiler-rs/runs plus the Vite dev server, in one
# terminal. Independent of any run — start it whenever, it reads the run
# directories from byte zero, so a run started an hour ago shows up whole and
# then keeps streaming.

# Watch local runs at http://localhost:5173 (backend + console dev server).
ui:
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill 0' EXIT
    just ui-server &
    just ui-web &
    wait

# Serve local runs as SSE at http://localhost:8080 (no auth, no AWS).
ui-server port='8080' runs_dir='':
    cargo run --quiet -p runner-live -- \
      --source local --no-auth --listen 127.0.0.1:{{ port }} \
      {{ if runs_dir == '' { '' } else { '--runs-dir ' + runs_dir } }}

# Console dev server alone; point it elsewhere with VITE_RUNNER_LIVE_URL.
ui-web:
    pnpm install --silent
    pnpm --filter @planetiler-rs/runner dev
