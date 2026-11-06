#!/usr/bin/env bash
# Pull the bot's image and (re)create its container if the image changed or it isn't running.
# Run every 2 minutes by nicenovember-update.timer (see install.sh). Safe to run any time.
set -euo pipefail

IMAGE=${NICENOVEMBER_IMAGE:-ghcr.io/sauce-2/nicenovember:latest}
NAME=${NICENOVEMBER_NAME:-nicenovember}
DIR=${NICENOVEMBER_DIR:-$HOME/nicenovember}

# Podman prints image ids with or without a "sha256:" prefix depending on the command.
bare() { sed 's/^sha256://'; }

podman pull -q "$IMAGE" >/dev/null
latest=$(podman image inspect --format '{{.Id}}' "$IMAGE" | bare)
current=$(podman container inspect --format '{{.Image}}' "$NAME" 2>/dev/null | bare || true)
running=$(podman container inspect --format '{{.State.Running}}' "$NAME" 2>/dev/null || true)

if [ "$latest" = "$current" ] && [ "$running" = true ]; then
  exit 0
fi

podman rm -f "$NAME" >/dev/null 2>&1 || true
podman run -d --name "$NAME" --restart=always \
  --env-file "$DIR/.env" \
  -v nicenovember-data:/data \
  "$IMAGE" >/dev/null
podman image prune -f >/dev/null 2>&1 || true
echo "nicenovember: running ${latest:0:12}"
