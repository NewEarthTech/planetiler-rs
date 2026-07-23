#!/usr/bin/env bash
# Launch a dedicated EC2 benchmark box, run a job on it, fetch results, and
# terminate it. The repo travels as a git bundle (no credentials leave the
# laptop); the instance schedules its own shutdown at boot (+MAX_HOURS, 4h for
# extract jobs, 14h for planet) so a wedged run can't idle for a weekend, and
# shutdown means terminate.
#
# Usage: parity/cloud/run.sh <job> <ref-a> <ref-b> [rounds] [instance-type] [max-hours] [data-ebs-gb]
#   e.g. parity/cloud/run.sh gb-ab main perf/parallel-merge
#        parity/cloud/run.sh europe-ab main perf/parallel-merge 2 r7g.8xlarge 8 500
#        parity/cloud/run.sh planet main
# `d`-suffix instance types use their local NVMe for /data; for types without
# instance store, pass data-ebs-gb to attach a gp3 data volume instead
# (provisioned to 8000 IOPS / 500 MB/s so disk class doesn't dominate results).
#
# The `planet` job is the odd one out and behaves differently on purpose: it
# takes a single ref (there is nothing to A/B against — nobody has run a planet
# yet), it defaults to a bigger box in us-west-2 because the OSM planet mirror
# lives there and an in-region fetch of 94 GB is free and fast, and it
# **detaches after dispatch** instead of polling. A planet run is hours long,
# and holding a laptop ssh session open across that has flaked repeatedly.
# Watch and collect it with `watch.sh <ip>` instead.
set -euo pipefail

JOB=${1:?job name (gb-ab | europe-ab | planet)}
REF_A=${2:?baseline ref}
case "$JOB" in
  planet)
    REF_B=${3:-$REF_A}
    ROUNDS=${4:-1}
    INSTANCE_TYPE=${5:-r7gd.8xlarge}
    MAX_HOURS=${6:-14}
    DATA_EBS_GB=${7:-0}
    REGION=${AWS_REGION:-us-west-2}
    ;;
  *)
    REF_B=${3:?candidate ref}
    ROUNDS=${4:-3}
    INSTANCE_TYPE=${5:-r7gd.4xlarge}
    MAX_HOURS=${6:-4}
    DATA_EBS_GB=${7:-0}
    REGION=${AWS_REGION:-us-east-1}
    ;;
esac

# Pinned rather than `planet-latest`: a benchmark input has to be reproducible,
# same reason the upstream jar is pinned. Bump deliberately, and re-record.
PLANET_URL=${PLANET_URL:-https://osm-planet-us-west-2.s3.dualstack.us-west-2.amazonaws.com/planet/pbf/2026/planet-260713.osm.pbf}
NAME=planetiler-bench
KEY_FILE=$HOME/.ssh/$NAME.pem
HERE=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$HERE/../.." && pwd)
OUT_DIR="$HERE/results"
AWS() { aws --region "$REGION" "$@"; }

echo "== resolve AMI (ubuntu 24.04 arm64)"
AMI=$(AWS ssm get-parameter \
  --name /aws/service/canonical/ubuntu/server/24.04/stable/current/arm64/hvm/ebs-gp3/ami-id \
  --query Parameter.Value --output text)
echo "ami: $AMI"

echo "== key pair"
if ! AWS ec2 describe-key-pairs --key-names "$NAME" >/dev/null 2>&1; then
  AWS ec2 create-key-pair --key-name "$NAME" --key-type ed25519 \
    --query KeyMaterial --output text > "$KEY_FILE"
  chmod 600 "$KEY_FILE"
fi
[ -f "$KEY_FILE" ] || { echo "EC2 has key '$NAME' but $KEY_FILE is missing locally" >&2; exit 1; }

echo "== security group (ssh from this IP only)"
MYIP=$(curl -fsS https://checkip.amazonaws.com)
SG=$(AWS ec2 describe-security-groups --filters "Name=group-name,Values=$NAME" \
  --query 'SecurityGroups[0].GroupId' --output text)
if [ "$SG" = "None" ] || [ -z "$SG" ]; then
  SG=$(AWS ec2 create-security-group --group-name "$NAME" \
    --description "planetiler bench ssh" --query GroupId --output text)
fi
AWS ec2 authorize-security-group-ingress --group-id "$SG" \
  --protocol tcp --port 22 --cidr "$MYIP/32" >/dev/null 2>&1 || true

echo "== launch $INSTANCE_TYPE"
USER_DATA="#!/bin/bash
shutdown -h +$((MAX_HOURS * 60))
"'# /data: prefer local instance-store NVMe; else the attached data EBS volume
# (the disk with no partitions and nothing mounted).
dev=$(lsblk -dno NAME,MODEL | awk "/Instance Storage/{print \"/dev/\"\$1; exit}")
if [ ! -b "$dev" ]; then
  for d in $(lsblk -pdno NAME,TYPE | awk "\$2==\"disk\"{print \$1}"); do
    [ -z "$(lsblk -no MOUNTPOINTS "$d" | tr -d "[:space:]")" ] && dev=$d && break
  done
fi
[ -b "$dev" ] || exit 1
mkfs.ext4 -F "$dev"
mkdir -p /data && mount "$dev" /data && chown ubuntu:ubuntu /data
'
MAPPINGS='DeviceName=/dev/sda1,Ebs={VolumeSize=30,VolumeType=gp3,DeleteOnTermination=true}'
if [ "$DATA_EBS_GB" -gt 0 ]; then
  MAPPINGS="$MAPPINGS DeviceName=/dev/sdb,Ebs={VolumeSize=$DATA_EBS_GB,VolumeType=gp3,Iops=8000,Throughput=500,DeleteOnTermination=true}"
fi
# shellcheck disable=SC2086
IID=$(AWS ec2 run-instances \
  --image-id "$AMI" --instance-type "$INSTANCE_TYPE" \
  --key-name "$NAME" --security-group-ids "$SG" \
  --instance-initiated-shutdown-behavior terminate \
  --block-device-mappings $MAPPINGS \
  --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$NAME},{Key=job,Value=$JOB}]" \
  --user-data "$USER_DATA" \
  --query 'Instances[0].InstanceId' --output text)
echo "instance: $IID"

FETCHED=0
cleanup() {
  if [ "$FETCHED" = 1 ]; then
    echo "terminating $IID"
    AWS ec2 terminate-instances --instance-ids "$IID" >/dev/null || true
  else
    echo "leaving $IID running for inspection (self-terminates within ${MAX_HOURS}h); kill it now with:"
    echo "  aws --region $REGION ec2 terminate-instances --instance-ids $IID"
  fi
}
trap cleanup EXIT

AWS ec2 wait instance-running --instance-ids "$IID"
IP=$(AWS ec2 describe-instances --instance-ids "$IID" \
  --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)
echo "ip: $IP"
SSH_OPTS=(-i "$KEY_FILE" -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 -o ServerAliveInterval=30)
for _ in $(seq 1 40); do ssh "${SSH_OPTS[@]}" "ubuntu@$IP" true 2>/dev/null && break; sleep 5; done
ssh "${SSH_OPTS[@]}" "ubuntu@$IP" true || { echo "ssh unreachable" >&2; exit 1; }

echo "== ship repo bundle + runner"
BUNDLE=$(mktemp -t planetiler-bundle.XXXXXX)
if [ "$JOB" = planet ]; then
  git -C "$REPO_ROOT" bundle create "$BUNDLE" "$REF_A" 2>/dev/null
else
  git -C "$REPO_ROOT" bundle create "$BUNDLE" "$REF_A" "$REF_B" 2>/dev/null
fi
scp -q "${SSH_OPTS[@]}" "$BUNDLE" "ubuntu@$IP:repo.bundle"
RUNNER=remote.sh
[ "$JOB" = planet ] && RUNNER=remote-planet.sh
scp -q "${SSH_OPTS[@]}" "$HERE/$RUNNER" "ubuntu@$IP:$RUNNER"
rm -f "$BUNDLE"

# nohup with stdin still attached leaves the dispatching ssh hanging; the
# `< /dev/null` is load-bearing, not decoration.
if [ "$JOB" = planet ]; then
  echo "== start job: planet $REF_A"
  ssh "${SSH_OPTS[@]}" "ubuntu@$IP" \
    "chmod +x $RUNNER && nohup ./$RUNNER '$REF_A' '$PLANET_URL' > job.log 2>&1 < /dev/null & echo dispatched"
  # Drop the cleanup trap outright rather than flipping FETCHED: the box has to
  # outlive this script, and neither branch of cleanup is what we want here.
  trap - EXIT
  cat <<EOS

== detached. The box is running the planet job; this script is done.

  instance: $IID   ip: $IP   region: $REGION
  failsafe: self-terminates ${MAX_HOURS}h after boot, shutdown = terminate

  watch + collect:  $HERE/watch.sh $IP $MAX_HOURS
  peek at the log:  ssh -i $KEY_FILE ubuntu@$IP 'tail -f job.log'
  kill it now:      aws --region $REGION ec2 terminate-instances --instance-ids $IID

EOS
  exit 0
fi

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
