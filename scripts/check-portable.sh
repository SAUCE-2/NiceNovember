#!/usr/bin/env bash
# Run the image's bot, emulating a CPU without AVX-512 or AMX (QEMU's Haswell: AVX2 at most), up to
# the point where it has loaded the Whisper model. Fails if it dies with an illegal instruction,
# which is what a whisper.cpp tuned for the build machine does on the Steam Machine.
# Usage: scripts/check-portable.sh IMAGE   (needs qemu-user-static installed on the host)
set -euo pipefail
IMAGE=${1:?usage: $0 IMAGE}
QEMU=$(command -v qemu-x86_64-static) || { echo "install qemu-user-static" >&2; exit 2; }

set +e
out=$(timeout 300 docker run --rm -v "$QEMU:/qemu:ro" --entrypoint /qemu \
  -e DISCORD_TOKEN=fake -e RUST_LOG=info \
  "$IMAGE" -cpu Haswell /usr/local/bin/nicenovember 2>&1)
code=$?
set -e

if [ "$code" = 132 ] || grep -q "Illegal instruction" <<<"$out"; then
  echo "FAIL: illegal instruction on a CPU without AVX-512 (exit $code)" >&2
  grep -v "^whisper_\|^ggml" <<<"$out" | tail -5 >&2
  exit 1
fi
# Logged right after the model loads, before connecting to Discord.
if ! grep -q "JEV_API_KEY not set" <<<"$out"; then
  echo "FAIL: didn't get past loading the Whisper model (exit $code)" >&2
  tail -20 <<<"$out" >&2
  exit 1
fi
echo "portable: loads Whisper on a Haswell-class CPU"
