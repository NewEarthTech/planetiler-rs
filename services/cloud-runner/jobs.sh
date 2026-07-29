#!/usr/bin/env bash
# The job catalogue: the one place that says what a named run reads and which
# flags it renders with. Sourced, never executed.
#
# Why it exists: a local run and a cloud run of the same job have to be
# comparable 1:1, and they can only be that if the input URL and the CLI flags
# come from a single definition. Before this file `gb-ab` lived in remote.sh
# and `planet` in remote-planet.sh, so a laptop reproduction was a copy-paste
# of a flag list that had already drifted twice. Now every entry point --
# `just run <job>` on a laptop, remote.sh and remote-planet.sh on a bench box --
# calls job_spec and renders with whatever it sets.
#
#   . services/cloud-runner/jobs.sh
#   job_spec planet
#   "$BIN" generate --source "$INPUT" --output "$OUT" "${JOB_FLAGS[@]}"
#
# job_spec <job> sets:
#   JOB_NAME        the job, echoed back
#   JOB_INPUT_URL   where the .osm.pbf comes from (env-overridable, see below)
#   JOB_INPUT_FILE  cache filename for the staged input
#   JOB_FLAGS       the planetiler-rs generate flags, as an array
#   JOB_KIND        single | ab  (ab jobs interleave two refs; see remote.sh)
#   JOB_IDENTITY    for ab jobs: hash (full content) | signature (per-tile length)
#   JOB_SCALE       laptop | box   -- advisory; laptop jobs fit on a dev machine
#
# bash 3.2 compatible (macOS ships it): indexed arrays only, no associatives.

# The planet input is pinned rather than `planet-latest`, for the same reason
# the upstream jar is pinned: a benchmark input has to be reproducible. Bump
# deliberately and re-record. run-api passes its own copy through PLANET_URL so
# the console dispatch keeps the server-side pin authoritative.
PLANET_URL=${PLANET_URL:-https://osm-planet-us-west-2.s3.dualstack.us-west-2.amazonaws.com/planet/pbf/2026/planet-260713.osm.pbf}

GEOFABRIK=${GEOFABRIK:-https://download.geofabrik.de}

job_spec() {
  JOB_NAME=${1:?job name}
  JOB_KIND=single
  JOB_IDENTITY=none
  JOB_SCALE=box
  # The shared rendering shape: all OSM features into one `osm` layer, à la
  # Planetiler's osm-qa, with simplification and buffering off so the output is
  # a function of the input alone. Per-job overrides follow.
  JOB_FLAGS=(--layer osm --minzoom 12 --maxzoom 12
    --storage sparse --min-size 0 --simplify 0 --buffer 0 --force)

  case "$JOB_NAME" in
    monaco)
      # The smoke test: 0.7 MB, finishes in under a second, exercises every
      # stage. Zooms go to 14 because at 12 monaco is four tiles and a merge
      # phase you cannot see is a merge phase you cannot debug.
      JOB_INPUT_URL=$GEOFABRIK/europe/monaco-latest.osm.pbf
      JOB_INPUT_FILE=monaco.osm.pbf
      JOB_FLAGS=(--layer osm --minzoom 12 --maxzoom 14
        --storage sparse --min-size 0 --simplify 0 --buffer 0 --force)
      JOB_SCALE=laptop
      ;;
    rhode-island)
      # 49 MB: the largest input that still behaves like an interactive command
      # on a laptop (~4 s, ~1.1 GB), and the smallest that spills the sort.
      JOB_INPUT_URL=$GEOFABRIK/north-america/us/rhode-island-latest.osm.pbf
      JOB_INPUT_FILE=rhode-island.osm.pbf
      JOB_FLAGS=(--layer osm --minzoom 12 --maxzoom 14
        --storage sparse --min-size 0 --simplify 0 --buffer 0 --force)
      JOB_SCALE=laptop
      ;;
    great-britain | gb-ab)
      JOB_INPUT_URL=$GEOFABRIK/europe/great-britain-latest.osm.pbf
      JOB_INPUT_FILE=great-britain.osm.pbf
      # Small enough to prove byte-identity by hashing the whole archive.
      if [ "$JOB_NAME" = gb-ab ]; then JOB_KIND=ab; JOB_IDENTITY=hash; fi
      ;;
    europe | europe-ab)
      JOB_INPUT_URL=$GEOFABRIK/europe-latest.osm.pbf
      JOB_INPUT_FILE=europe.osm.pbf
      # Too big to hash both archives; compare a per-tile (coord, length)
      # signature of the outputs the timed rounds already produced.
      if [ "$JOB_NAME" = europe-ab ]; then JOB_KIND=ab; JOB_IDENTITY=signature; fi
      ;;
    planet)
      JOB_INPUT_URL=$PLANET_URL
      JOB_INPUT_FILE=planet.osm.pbf
      # --sort-memory-mb 1024 is not a tuning preference: the 256 MB default
      # spills ~1200 chunks and the k-way merge opens every one at once
      # (bench/results/planet-preflight.md).
      JOB_FLAGS=(--layer osm --minzoom 12 --maxzoom 12
        --storage sparse --sort-memory-mb 1024
        --min-size 0 --simplify 0 --buffer 0 --force)
      ;;
    *)
      echo "unknown job: $JOB_NAME (try: $(job_names))" >&2
      return 1
      ;;
  esac
  # Explicit: callers run under `set -e`, where a function returning the
  # status of whatever happened to be last would exit them without a word.
  return 0
}

job_names() { echo "monaco rhode-island great-britain gb-ab europe europe-ab planet"; }

# Flags as one line, for reports and for the console's run header — the record
# of what a number was measured with.
job_flags_line() { echo "${JOB_FLAGS[@]}"; }
