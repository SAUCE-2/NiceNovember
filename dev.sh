#!/usr/bin/env bash
# Run cargo inside the dev toolchain image (has cmake/clang/opus for whisper + songbird).
# Usage: ./dev.sh cargo check
set -euo pipefail
docker image inspect nicenovember-toolchain >/dev/null 2>&1 || docker build -q -t nicenovember-toolchain -f docker/dev.Dockerfile docker
exec docker run --rm -t -v "$PWD":/app -w /app \
  -v nicenovember-cargo-registry:/usr/local/cargo/registry \
  -v nicenovember-target:/app/target \
  --env-file <(test -f .env && cat .env || true) \
  nicenovember-toolchain "$@"
