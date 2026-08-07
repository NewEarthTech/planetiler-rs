#!/usr/bin/env bash
# Runs ON the benchmark instance (launched by run.sh). Installs deps, stages
# input on the local NVMe, builds both refs from the shipped git bundle, checks
# byte-identity between them, then runs an interleaved A/B and writes a results
# report. Writes `status` = DONE/FAIL for the laptop-side poller.
set -euo pipefail

JOB=${1:?job name (gb-ab | europe-ab)}
REF_A=${2:?baseline ref}
REF_B=${3:?candidate ref}
ROUNDS=${4:-3}

cd "$HOME"
echo RUNNING > status

# Ship results/ + the job log to a run-keyed S3 prefix before the box dies —
# the collection path for a console-dispatched box, which has no ssh for the
# laptop pollers to scp from (#59). Runs on FAIL too: a failed run's logs are
# the valuable part. Best-effort by design: no RUN_ID in the dispatch env (an
# old-style dispatch) or no instance profile and the job's outcome is
# unchanged — the laptop path still collects by scp either way.
ship_results() {
  [ -n "${RUN_ID:-}" ] || return 0
  # aws via snap, same reasoning as drain.sh (apt may not own the box's
  # locks). `|| true` so a failed install can't errexit the EXIT trap.
  command -v aws >/dev/null || sudo snap install aws-cli --classic >/dev/null 2>&1 || true
  command -v aws >/dev/null || { echo "no aws cli; results stay on the box"; return 0; }
  local dest="s3://${RESULTS_BUCKET:-planetiler-rs-run-results}/$RUN_ID"
  local S3=(aws --region "${RESULTS_REGION:-us-east-1}" s3 cp --no-progress)
  if [ -d results ] && "${S3[@]}" --recursive results/ "$dest/results/" >/dev/null 2>&1; then
    echo "results shipped: $dest/results/"
  else
    echo "results ship to $dest failed (no instance profile?)"
  fi
  # Last, so the copy holds every line above; the drain has the rest.
  "${S3[@]}" job.log "$dest/job.log" >/dev/null 2>&1 || true
}

# On any exit that didn't reach DONE — set -e failure, explicit exit 1, signal
# — mark FAIL so the laptop-side poller stops instead of watching RUNNING
# forever.
finalize() {
  [ "$(cat status 2>/dev/null)" = DONE ] || echo FAIL > status
  ship_results
}
trap finalize EXIT

# Job selects the input, the flags, and how identity is proven — all of it out
# of the shared catalogue, so this box renders exactly what `just run <job>`
# renders on a laptop. Big inputs skip the two dedicated identity runs and
# instead compare a cheap per-tile signature of the outputs the timed rounds
# already produced.
# shellcheck source=services/cloud-runner/jobs.sh
. "$(dirname "${BASH_SOURCE[0]}")/jobs.sh"
job_spec "$JOB"
[ "$JOB_KIND" = ab ] || { echo "job $JOB is not an A/B job" >&2; exit 1; }
URL=$JOB_INPUT_URL
IDENTITY=$JOB_IDENTITY

echo "== deps"
sudo apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
  build-essential protobuf-compiler sqlite3 time git curl >/dev/null
if ! command -v cargo >/dev/null; then
  curl -fsS https://sh.rustup.rs | sh -s -- -y --profile minimal -q
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"

echo "== instance-store scratch"
# user-data formats and mounts the NVMe instance store at /data; wait it out
for _ in $(seq 1 90); do mountpoint -q /data && break; sleep 2; done
mountpoint -q /data || { echo "no /data mount" >&2; exit 1; }
mkdir -p /data/tmp /data/out results
export TMPDIR=/data/tmp

echo "== input (geofabrik, length-verified)"
INPUT=/data/input.osm.pbf
# -L on the HEAD too: geofabrik redirects, and the pre-redirect response has
# no content-length; take the final hop's value.
EXPECT=$(curl -fsSIL "$URL" | tr -d '\r' | awk 'tolower($1)=="content-length:"{print $2}' | tail -1)
[ -n "$EXPECT" ] || { echo "no content-length from $URL" >&2; exit 1; }
if [ -f "$INPUT" ] && [ "$(stat -c %s "$INPUT")" = "$EXPECT" ]; then
  echo "input already present and complete ($EXPECT bytes); skipping download"
else
  curl -fsSL --retry 3 -o "$INPUT" "$URL"
fi
ACTUAL=$(stat -c %s "$INPUT")
[ "$ACTUAL" = "$EXPECT" ] || {
  echo "download size mismatch: got $ACTUAL want $EXPECT" >&2; exit 1; }

echo "== build $REF_A and $REF_B"
git clone -q repo.bundle repo
BIN_A="$HOME/ptrs-${REF_A//\//-}"
BIN_B="$HOME/ptrs-${REF_B//\//-}"
(cd repo && git checkout -q "$REF_A" && cargo build --release -q && cp target/release/planetiler-rs "$BIN_A")
(cd repo && git checkout -q "$REF_B" && cargo build --release -q && cp target/release/planetiler-rs "$BIN_B")

FLAGS=("${JOB_FLAGS[@]}")
REPORT="results/$JOB-$(date -u +%Y%m%d-%H%M%S).txt"

env_block() {
  TOKEN=$(curl -fsS -X PUT http://169.254.169.254/latest/api/token \
    -H 'X-aws-ec2-metadata-token-ttl-seconds: 60' 2>/dev/null || true)
  ITYPE=$(curl -fsS -H "X-aws-ec2-metadata-token: $TOKEN" \
    http://169.254.169.254/latest/meta-data/instance-type 2>/dev/null || echo unknown)
  echo "instance: $ITYPE  cores: $(nproc)  kernel: $(uname -sr)  arch: $(uname -m)"
  echo "rustc: $(rustc -V)"
  echo "input: $URL"
  echo "input bytes: $ACTUAL  sha256: $(sha256sum "$INPUT" | cut -c1-16)"
  echo "flags: $(job_flags_line)"
  echo "refs: $REF_A=$(git -C repo rev-parse --short "$REF_A")  $REF_B=$(git -C repo rev-parse --short "$REF_B")"
}

hash_tiles() { sqlite3 "$1" "SELECT zoom_level, tile_column, tile_row, hex(tile_data) FROM tiles ORDER BY 1,2,3;" | sha256sum | cut -c1-16; }
# Per-tile (coord, byte length) dump — no blobs, so it stays cheap at any
# scale, while still pinning tile set, order, and every tile's exact size.
sig_tiles()  { sqlite3 "$1" "SELECT zoom_level, tile_column, tile_row, length(tile_data) FROM tiles ORDER BY 1,2,3;" | sha256sum | cut -c1-16; }
hash_meta()  { sqlite3 "$1" "SELECT name, value FROM metadata ORDER BY name;" | sha256sum | cut -c1-16; }
row_counts() { sqlite3 "$1" "SELECT (SELECT count(*) FROM tiles_shallow) || '/' || (SELECT count(*) FROM tiles_data);"; }

compare_outputs() {
  local a=$1 b=$2 tiles_fn=$3 label=$4
  local ta tb ma mb ra rb
  ta=$($tiles_fn "$a"); tb=$($tiles_fn "$b")
  ma=$(hash_meta "$a");  mb=$(hash_meta "$b")
  ra=$(row_counts "$a"); rb=$(row_counts "$b")
  {
    echo "tiles($label): $ta vs $tb"
    echo "meta:  $ma vs $mb"
    echo "rows:  $ra vs $rb"
  } | tee -a "$REPORT"
  [ "$ta" = "$tb" ] && [ "$ma" = "$mb" ] && [ "$ra" = "$rb" ] || {
    echo "IDENTITY CHECK FAILED" | tee -a "$REPORT"; exit 1; }
}

{
  echo "# $JOB  $(date -u +%FT%TZ)"
  env_block
} | tee "$REPORT"

if [ "$IDENTITY" = hash ]; then
  echo "## byte-identity ($REF_A vs $REF_B)" | tee -a "$REPORT"
  "$BIN_A" generate --source "$INPUT" --output /data/out/a.mbtiles "${FLAGS[@]}" >/dev/null 2>&1
  "$BIN_B" generate --source "$INPUT" --output /data/out/b.mbtiles "${FLAGS[@]}" >/dev/null 2>&1
  compare_outputs /data/out/a.mbtiles /data/out/b.mbtiles hash_tiles full-content
  rm -f /data/out/*.mbtiles
fi

echo "== warm input cache" | tee -a "$REPORT"
cat "$INPUT" >/dev/null

{
  echo
  echo "## interleaved A/B ($ROUNDS rounds per side)"
} | tee -a "$REPORT"
for round in $(seq 1 "$ROUNDS"); do
  for side in A B; do
    [ "$side" = A ] && BIN=$BIN_A REF=$REF_A || BIN=$BIN_B REF=$REF_B
    echo "=== $REF round $round ===" | tee -a "$REPORT"
    /usr/bin/time -v "$BIN" generate --source "$INPUT" --output "/data/out/ab-$side.mbtiles" "${FLAGS[@]}" 2>&1 \
      | grep -E "stream \+ render|merge \+ encode|Elapsed \(wall|Maximum resident" \
      | sed -E 's/^[[:space:]]+//' | tee -a "$REPORT"
  done
done

if [ "$IDENTITY" = signature ]; then
  {
    echo
    echo "## output signature ($REF_A vs $REF_B, from the timed rounds)"
  } | tee -a "$REPORT"
  compare_outputs /data/out/ab-A.mbtiles /data/out/ab-B.mbtiles sig_tiles per-tile-length
fi

echo DONE > status
echo "report: $REPORT"
