#!/usr/bin/env bash
# Watch a detached bench box, stream its log, then fetch results and terminate.
#
# run.sh's gb-ab/europe-ab jobs poll inline because they finish inside an hour.
# The planet job detaches instead: it runs for hours, and a laptop-side ssh
# session held open that long has flaked repeatedly (sleep, wifi, VPN). This
# script is the collector, and it is safe to kill and re-run at any point —
# the job is nohup'd on the box and does not care whether anyone is watching.
#
# Usage: parity/cloud/watch.sh <ip> [max-hours] [poll-seconds]
set -euo pipefail

IP=${1:?instance ip}
MAX_HOURS=${2:-14}
POLL=${3:-60}
REGION=${AWS_REGION:-us-west-2}
NAME=planetiler-bench
KEY_FILE=$HOME/.ssh/$NAME.pem
HERE=$(cd "$(dirname "$0")" && pwd)
OUT_DIR="$HERE/results"
AWS() { aws --region "$REGION" "$@"; }
SSH_OPTS=(-i "$KEY_FILE" -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 -o ServerAliveInterval=30)

IID=$(AWS ec2 describe-instances \
  --filters "Name=ip-address,Values=$IP" "Name=instance-state-name,Values=running" \
  --query 'Reservations[0].Instances[0].InstanceId' --output text)
[ -n "$IID" ] && [ "$IID" != None ] || { echo "no running instance at $IP" >&2; exit 1; }
echo "watching $IID at $IP (region $REGION, poll ${POLL}s)"

LAST_LINES=0
STATUS=unknown
for _ in $(seq 1 $((MAX_HOURS * 3600 / POLL))); do
  # An unreachable box is not a failed job — the instance may be busy or the
  # local network may have blinked. Keep polling; only status says otherwise.
  STATUS=$(ssh "${SSH_OPTS[@]}" "ubuntu@$IP" 'cat status 2>/dev/null' || echo unreachable)
  TOTAL=$(ssh "${SSH_OPTS[@]}" "ubuntu@$IP" 'wc -l < job.log 2>/dev/null' || echo 0)
  if [ "$TOTAL" -gt "$LAST_LINES" ] 2>/dev/null; then
    ssh "${SSH_OPTS[@]}" "ubuntu@$IP" "sed -n '$((LAST_LINES + 1)),${TOTAL}p' job.log" || true
    LAST_LINES=$TOTAL
  fi
  case "$STATUS" in DONE|FAIL) break ;; esac
  sleep "$POLL"
done
echo "status: $STATUS"

echo "== fetch results"
mkdir -p "$OUT_DIR"
scp -q -r "${SSH_OPTS[@]}" "ubuntu@$IP:results/*" "$OUT_DIR/" || true
scp -q "${SSH_OPTS[@]}" "ubuntu@$IP:job.log" "$OUT_DIR/planet-$(date +%Y%m%d-%H%M%S).log" || true

if [ "$STATUS" = DONE ]; then
  echo "terminating $IID"
  AWS ec2 terminate-instances --instance-ids "$IID" >/dev/null || true
  echo "results in $OUT_DIR"
else
  echo "status=$STATUS — leaving $IID up for inspection (boot failsafe still armed)" >&2
  echo "  ssh -i $KEY_FILE ubuntu@$IP" >&2
  echo "  aws --region $REGION ec2 terminate-instances --instance-ids $IID" >&2
  exit 1
fi
