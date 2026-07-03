#!/usr/bin/env bash
# Re-dispatch a job on an existing (kept) bench instance — reuses the box, its
# downloaded input, and the shared key/SG — after a remote.sh fix. Ships a
# fresh bundle + runner, resets the boot failsafe clock, then polls, fetches
# results, and terminates exactly like run.sh.
# Usage: parity/cloud/redispatch.sh <ip> <job> <ref-a> <ref-b> [rounds] [max-hours]
set -euo pipefail

IP=${1:?instance ip}
JOB=${2:?job name}
REF_A=${3:?baseline ref}
REF_B=${4:?candidate ref}
ROUNDS=${5:-3}
MAX_HOURS=${6:-4}
REGION=${AWS_REGION:-us-east-1}
NAME=planetiler-bench
KEY_FILE=$HOME/.ssh/$NAME.pem
HERE=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$HERE/../.." && pwd)
OUT_DIR="$HERE/results"
AWS() { aws --region "$REGION" "$@"; }
SSH_OPTS=(-i "$KEY_FILE" -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 -o ServerAliveInterval=30)

IID=$(AWS ec2 describe-instances \
  --filters "Name=ip-address,Values=$IP" "Name=instance-state-name,Values=running" \
  --query 'Reservations[0].Instances[0].InstanceId' --output text)
[ -n "$IID" ] && [ "$IID" != None ] || { echo "no running instance at $IP" >&2; exit 1; }
echo "instance: $IID  ip: $IP"

FETCHED=0
cleanup() {
  if [ "$FETCHED" = 1 ]; then
    echo "terminating $IID"
    AWS ec2 terminate-instances --instance-ids "$IID" >/dev/null || true
  else
    echo "leaving $IID running for inspection (boot failsafe still armed); kill it now with:"
    echo "  aws --region $REGION ec2 terminate-instances --instance-ids $IID"
  fi
}
trap cleanup EXIT

echo "== reset previous job + failsafe clock"
ssh "${SSH_OPTS[@]}" "ubuntu@$IP" \
  'pkill -f "\./remote\.sh" 2>/dev/null; rm -rf status job.log repo; sudo shutdown -c 2>/dev/null; true'
ssh "${SSH_OPTS[@]}" "ubuntu@$IP" "sudo shutdown -h +$((MAX_HOURS * 60)) >/dev/null 2>&1"

echo "== ship repo bundle + runner"
BUNDLE=$(mktemp -t planetiler-bundle.XXXXXX)
git -C "$REPO_ROOT" bundle create "$BUNDLE" "$REF_A" "$REF_B" 2>/dev/null
scp -q "${SSH_OPTS[@]}" "$BUNDLE" "ubuntu@$IP:repo.bundle"
scp -q "${SSH_OPTS[@]}" "$HERE/remote.sh" "ubuntu@$IP:remote.sh"
rm -f "$BUNDLE"

echo "== start job: $JOB $REF_A $REF_B ($ROUNDS rounds)"
ssh "${SSH_OPTS[@]}" "ubuntu@$IP" \
  "chmod +x remote.sh && nohup ./remote.sh '$JOB' '$REF_A' '$REF_B' '$ROUNDS' > job.log 2>&1 < /dev/null & echo dispatched"

echo "== poll (status + log tail)"
LAST_LINES=0
for _ in $(seq 1 $((MAX_HOURS * 120))); do
  sleep 30
  STATUS=$(ssh "${SSH_OPTS[@]}" "ubuntu@$IP" 'cat status 2>/dev/null' || echo unreachable)
  TOTAL=$(ssh "${SSH_OPTS[@]}" "ubuntu@$IP" 'wc -l < job.log 2>/dev/null' || echo 0)
  if [ "$TOTAL" -gt "$LAST_LINES" ] 2>/dev/null; then
    ssh "${SSH_OPTS[@]}" "ubuntu@$IP" "sed -n '$((LAST_LINES + 1)),${TOTAL}p' job.log" || true
    LAST_LINES=$TOTAL
  fi
  [ "$STATUS" = DONE ] || [ "$STATUS" = FAIL ] && break
done
echo "status: $STATUS"

echo "== fetch results"
mkdir -p "$OUT_DIR"
scp -q -r "${SSH_OPTS[@]}" "ubuntu@$IP:results/*" "$OUT_DIR/" && FETCHED=1
scp -q "${SSH_OPTS[@]}" "ubuntu@$IP:job.log" "$OUT_DIR/$JOB-$(date +%Y%m%d-%H%M%S).log" || true
[ "$STATUS" = DONE ] || { echo "job did not complete cleanly; instance kept for inspection" >&2; FETCHED=0; exit 1; }
echo "results in $OUT_DIR"
