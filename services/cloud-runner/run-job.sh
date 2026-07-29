#!/usr/bin/env bash
# The one entry point for a run, on either side of the fence. `just run <job>`
# calls this on a laptop; the bench box calls this from its dispatch (run.sh
# over ssh, run-api over SSM). Same command, same job spec out of jobs.sh --
# so a local run and a cloud run of the same job differ in machine, not input.
#
# What differs is decided here, by where the script finds itself, not by which
# command was typed:
#
#   cloud  -- an EC2 bench box (IMDS answers, or a .box-ready marker is
#             present). Hands off to remote.sh / remote-planet.sh, which own
#             the box-side contract: /data instance-store scratch, the memory
#             watchdog, the CloudWatch drain, results shipped to S3 before the
#             box dies. Unchanged behavior; this file only chooses it.
#   local  -- anything else. Hands off to local-run.sh: files under
#             ~/.planetiler-rs, no AWS credentials touched, no drain, and S3
#             only if you ask for it.
#
# Usage: run-job.sh <job> [ref-a] [ref-b] [rounds] [options]
#   just run monaco                 # laptop smoke run
#   just run planet                 # laptop planet run (you own the disk)
#   just run planet --save-s3       # …and ship the results home afterwards
#   PTRS_ENV=local just run monaco  # force the local path on a box
#
# Options (local runs; passed through to local-run.sh):
#   --save-s3 [s3://bucket]  ship results + logs after the run (default: off)
#   --run-id <id>            name the run instead of <job>-<utc stamp>
#   --no-build               use an existing target/release/planetiler-rs
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=services/cloud-runner/jobs.sh
. "$HERE/jobs.sh"

JOB=${1:-}
if [ -z "$JOB" ] || [ "$JOB" = -h ] || [ "$JOB" = --help ]; then
  sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  echo "jobs: $(job_names)"
  exit 0
fi
shift
job_spec "$JOB"

# Positional refs are a cloud A/B concern; local runs label themselves from the
# working tree. Options (--save-s3 and friends) are recognised in either
# position so `just run planet --save-s3` reads the way it looks.
REFS=()
OPTS=()
while [ $# -gt 0 ]; do
  case "$1" in
    # Options that take a value keep it with them, so it can't be mistaken
    # for a ref.
    --ref | --run-id)
      OPTS+=("$1" "${2:?$1 needs a value}"); shift 2 ;;
    --save-s3)
      OPTS+=("$1"); shift
      if [ "${1:-}" != "" ] && [ "${1#s3://}" != "${1:-}" ]; then OPTS+=("$1"); shift; fi ;;
    --*) OPTS+=("$1"); shift ;;
    *) REFS+=("$1"); shift ;;
  esac
done

# Where are we? PTRS_ENV wins (tests, and forcing a local run on a box);
# otherwise the box tells on itself: the requisition marker, or IMDS answering
# inside the one-second timeout it always answers within on EC2 and never
# answers within anywhere else.
detect_env() {
  [ -n "${PTRS_ENV:-}" ] && { echo "$PTRS_ENV"; return; }
  [ -f /home/ubuntu/.box-ready ] && { echo cloud; return; }
  if curl -fsS -m 1 -X PUT http://169.254.169.254/latest/api/token \
       -H 'X-aws-ec2-metadata-token-ttl-seconds: 60' >/dev/null 2>&1; then
    echo cloud
  else
    echo local
  fi
}
ENV=$(detect_env)

case "$ENV" in
  cloud)
    [ ${#OPTS[@]} -eq 0 ] || echo "note: ${OPTS[*]} ignored on a bench box (cloud runs always drain and ship)" >&2
    cd "$HERE"
    if [ "$JOB_KIND" = ab ]; then
      REF_A=${REFS[0]:?ab job needs a baseline ref}
      REF_B=${REFS[1]:?ab job needs a candidate ref}
      ROUNDS=${REFS[2]:-3}
      exec ./remote.sh "$JOB" "$REF_A" "$REF_B" "$ROUNDS"
    fi
    [ "$JOB" = planet ] || {
      echo "job '$JOB' has no box-side runner (single-ref cloud runs are planet-only today)" >&2
      exit 1; }
    exec ./remote-planet.sh "${REFS[0]:-main}" "$JOB_INPUT_URL"
    ;;
  local)
    LOCAL_ARGS=("$JOB")
    if [ ${#REFS[@]} -gt 0 ]; then LOCAL_ARGS+=(--ref "${REFS[0]}"); fi
    exec "$HERE/local-run.sh" "${LOCAL_ARGS[@]}" ${OPTS[@]+"${OPTS[@]}"}
    ;;
  *)
    echo "PTRS_ENV=$ENV is neither cloud nor local" >&2
    exit 1
    ;;
esac
