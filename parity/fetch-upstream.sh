#!/usr/bin/env bash
# Fetch the pinned upstream Planetiler release jar for head-to-head benchmarks.
# See parity/README.md. Requires: curl, shasum (or sha256sum).
set -euo pipefail

PLANETILER_VERSION="${PLANETILER_VERSION:-v0.10.2}"
DEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/upstream"
JAR="${DEST_DIR}/planetiler-${PLANETILER_VERSION}.jar"
BASE="https://github.com/onthegomap/planetiler/releases/download/${PLANETILER_VERSION}"

mkdir -p "${DEST_DIR}"
echo "Downloading Planetiler ${PLANETILER_VERSION} -> ${JAR}"
curl -fSL "${BASE}/planetiler.jar" -o "${JAR}"
curl -fSL "${BASE}/planetiler.jar.sha256" -o "${JAR}.sha256"

echo "Verifying checksum..."
expected="$(cut -d' ' -f1 < "${JAR}.sha256")"
if command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "${JAR}" | cut -d' ' -f1)"
else
  actual="$(sha256sum "${JAR}" | cut -d' ' -f1)"
fi

if [[ "${expected}" != "${actual}" ]]; then
  echo "Checksum mismatch: expected ${expected}, got ${actual}" >&2
  exit 1
fi
echo "OK: ${JAR}"
