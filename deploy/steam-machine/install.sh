#!/usr/bin/env bash
# Install NiceNovember on a Steam Machine (SteamOS), or any Linux box with rootless Podman 4.4+ and
# systemd. The bot runs from its GHCR image as a Quadlet service, and podman-auto-update keeps it
# on the latest image (Watchtower can't recreate containers under Podman: "cannot set memory
# swappiness with cgroupv2").
#
#   curl -fsSL https://raw.githubusercontent.com/SAUCE-2/NiceNovember/main/deploy/steam-machine/install.sh | bash
#
# The first run creates ~/nicenovember/.env for you to fill in; run it again afterwards.
# Safe to re-run: it only (re)writes its own files and units.
set -euo pipefail

IMAGE=${NICENOVEMBER_IMAGE:-ghcr.io/sauce-2/nicenovember:latest}
DIR=${NICENOVEMBER_DIR:-$HOME/nicenovember}
UNITS=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
QUADLETS=${XDG_CONFIG_HOME:-$HOME/.config}/containers/systemd
RAW=${NICENOVEMBER_RAW:-https://raw.githubusercontent.com/SAUCE-2/NiceNovember/main}
# The port the dashboard is on, on this machine.
PORT=${NICENOVEMBER_PORT:-8080}
# The checkout this script is in, or empty when it's piped in from curl.
HERE=
if [ -f "${BASH_SOURCE[0]:-}" ]; then
  HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
fi

die() { echo "error: $*" >&2; exit 1; }

# A file from this checkout if we're running from one, otherwise from GitHub.
fetch() {
  if [ -n "$HERE" ] && [ -f "$HERE/$1" ]; then
    cp "$HERE/$1" "$2"
  else
    curl -fsSL "$RAW/$3" -o "$2"
  fi
}

command -v podman >/dev/null || die "podman isn't installed (SteamOS 3.5+ ships it)"
mkdir -p "$DIR" "$UNITS/podman-auto-update.timer.d" "$QUADLETS"

if [ ! -f "$DIR/.env" ]; then
  fetch ../../.env.example "$DIR/.env" .env.example
  chmod 600 "$DIR/.env"
  echo "Created $DIR/.env. Fill in DISCORD_TOKEN and JEV_API_KEY, then run this again."
  exit 0
fi
grep -q '^DISCORD_TOKEN=..*' "$DIR/.env" || die "DISCORD_TOKEN is empty in $DIR/.env"
grep -q '^JEV_API_KEY=..*' "$DIR/.env" || echo "warning: JEV_API_KEY is empty, so nothing will be judged"

# Older installs ran the container from a oneshot timer service, which killed the container's
# fuse-overlayfs on exit and so crashed the bot with SIGBUS. The Quadlet service below replaces it.
systemctl --user disable --now nicenovember-update.timer >/dev/null 2>&1 || true
rm -f "$UNITS/nicenovember-update.service" "$UNITS/nicenovember-update.timer" "$DIR/update.sh"

# Where /log dashboard links point. Without it they'd say localhost, which only works on this
# machine. Its name works from the LAN and over Tailscale's MagicDNS; change it if you reach it
# another way.
if ! grep -q '^DASHBOARD_URL=..*' "$DIR/.env"; then
  sed -i '/^DASHBOARD_URL=/d' "$DIR/.env"
  echo "DASHBOARD_URL=http://$(hostname):$PORT" >>"$DIR/.env"
fi

cat > "$QUADLETS/nicenovember.container" <<UNIT
[Unit]
Description=NiceNovember Discord bot

[Container]
Image=$IMAGE
ContainerName=nicenovember
EnvironmentFile=$DIR/.env
Volume=nicenovember-data:/data
PublishPort=$PORT:8080
AutoUpdate=registry

[Service]
Restart=always
# The first start pulls the image.
TimeoutStartSec=900

[Install]
WantedBy=default.target
UNIT

cat > "$UNITS/podman-auto-update.timer.d/nicenovember.conf" <<UNIT
# Check for a new image every 2 minutes instead of daily.
[Timer]
OnCalendar=
OnCalendar=*:0/2
RandomizedDelaySec=0
UNIT

systemctl --user daemon-reload
systemctl --user enable --now podman-auto-update.timer
systemctl --user restart nicenovember.service

# Keep user services running when nobody's logged in (e.g. in Game Mode).
if [ "$(loginctl show-user "$USER" -p Linger --value 2>/dev/null)" != yes ]; then
  echo "Enabling linger so the bot keeps running after logout (asks for your sudo password)..."
  sudo loginctl enable-linger "$USER" ||
    echo "warning: couldn't enable linger; run 'sudo loginctl enable-linger $USER' yourself"
fi

echo "Done. Dashboard: $(sed -n 's/^DASHBOARD_URL=//p' "$DIR/.env")  Logs: podman logs -f nicenovember"
