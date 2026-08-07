# syntax=docker/dockerfile:1
#
# Runner image for planetiler-rs cloud dispatch: the release CLI plus the
# box-side job scripts on a minimal glibc base, nothing else. Built and pushed
# to ECR (`planetiler-rs-bench`) by .github/workflows/image.yml on merges to
# main; a console-dispatched bench box will `docker pull` this instead of
# compiling from source on the box the way services/cloud-runner/remote*.sh do
# today. arm64 only — the bench boxes are Graviton (r7g/r7gd), and the image
# workflow builds it natively there.
#
# The build context is the repo root, so build.rs finds the cargo workspace and
# the vendored proto:  docker build -f services/cloud-runner/Dockerfile .

FROM rust:1-bookworm AS build
# prost-build (build.rs) shells out to protoc; the C toolchain that rusqlite and
# mimalloc (both features = ["bundled"], compiled from source) need already
# ships in the rust image.
RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
# Cache the cargo registry and target dir so local rebuilds are cheap. The
# binary is copied out of the target cache mount in the same step, because a
# cache mount is not part of the resulting image layer. rust-toolchain.toml
# pins channel = "stable", which rustup honours here.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked --bin planetiler-rs \
    && cp target/release/planetiler-rs /usr/local/bin/planetiler-rs

FROM debian:bookworm-slim AS runtime
# The binary links only glibc/libgcc/libm (sqlite and mimalloc are compiled in,
# flate2 uses its pure-Rust backend, and nothing does TLS). ca-certificates is
# belt-and-suspenders for any future entrypoint that reaches the network.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/planetiler-rs /usr/local/bin/planetiler-rs
# The box-side job scripts ride next to the CLI so they version with the image
# sha instead of traveling from a laptop working tree by scp (#59). The scripts
# run on the HOST, not in the container (they mount /data, apt-get, sudo): a
# console-dispatched box lifts them out with
#   c=$(docker create <image>) && docker cp "$c:/opt/cloud-runner/." "$HOME" && docker rm "$c"
# The laptop-side dispatchers (run.sh, watch.sh, redispatch.sh) stay out — they
# never run on a box.
# run-job.sh + jobs.sh ride along as the box's entry point and job catalogue:
# dispatch calls `./run-job.sh <job> <ref>`, the same command a laptop reaches
# through `just run`, and it picks the box-side runner from where it finds
# itself. local-run.sh deliberately stays out — a box never takes that branch.
COPY services/cloud-runner/run-job.sh \
     services/cloud-runner/jobs.sh \
     services/cloud-runner/remote.sh \
     services/cloud-runner/remote-planet.sh \
     services/cloud-runner/tiles-delta.sh \
     services/cloud-runner/drain.sh /opt/cloud-runner/
ENTRYPOINT ["planetiler-rs"]
CMD ["--help"]
