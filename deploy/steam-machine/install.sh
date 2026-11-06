#!/usr/bin/env bash
# Install NiceNovember on a Steam Machine (SteamOS), or any Linux box with rootless Podman and
# systemd. The bot runs from its GHCR image and a systemd timer keeps it updated (Watchtower
# can't recreate containers under Podman: "cannot set memory swappiness with cgroupv2").
#
#   curl -fsSL https://raw.githubusercontent.com/SAUCE-2/NiceNovember/main/deploy/steam-machine/install.sh | bash
#
# The first run creates ~/nicenovember/.env for you to fill in; run it again afterwards.
# Safe to re-run: it only (re)writes its own files and units.
set -euo pipefail

IMAGE=${NICENOVEMBER_IMAGE:-ghcr.io/sauce-2/nicenovember:latest}
DIR=${NICENOVEMBER_DIR:-$HOME/nicenovember}
UNITS=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
RAW=${NICENOVEMBER_RAW:-https://raw.githubusercontent.com/SAUCE-2/NiceNovember/main}
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
mkdir -p "$DIR" "$UNITS"

if [ ! -f "$DIR/.env" ]; then
  fetch ../../.env.example "$DIR/.env" .env.example
  chmod 600 "$DIR/.env"
  echo "Created $DIR/.env. Fill in DISCORD_TOKEN and JEV_API_KEY, then run this again."
  exit 0
fi
grep -q '^DISCORD_TOKEN=..*' "$DIR/.env" || die "DISCORD_TOKEN is empty in $DIR/.env"
grep -q '^JEV_API_KEY=..*' "$DIR/.env" || echo "warning: JEV_API_KEY is empty, so nothing will be judged"

fetch update.sh "$DIR/update.sh" deploy/steam-machine/update.sh
chmod +x "$DIR/update.sh"

cat > "$UNITS/nicenovember-update.service" <<UNIT
[Unit]
Description=Update and (re)start the NiceNovember Discord bot
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
Environment=NICENOVEMBER_IMAGE=$IMAGE
Environment=NICENOVEMBER_DIR=$DIR
ExecStart=$DIR/update.sh
UNIT

cat > "$UNITS/nicenovember-update.timer" <<UNIT
[Unit]
Description=Check for NiceNovember updates every 2 minutes

[Timer]
OnBootSec=1min
OnUnitActiveSec=2min

[Install]
WantedBy=timers.target
UNIT

systemctl --user daemon-reload
# Starts containers with --restart=always again after a reboot.
systemctl --user enable --now podman-restart.service >/dev/null 2>&1 ||
  echo "warning: podman-restart.service isn't available; the timer still starts the bot after boot"
systemctl --user enable --now nicenovember-update.timer

# Keep user services running when nobody's logged in (e.g. in Game Mode).
if [ "$(loginctl show-user "$USER" -p Linger --value 2>/dev/null)" != yes ]; then
  echo "Enabling linger so the bot keeps running after logout (asks for your sudo password)..."
  sudo loginctl enable-linger "$USER" ||
    echo "warning: couldn't enable linger; run 'sudo loginctl enable-linger $USER' yourself"
fi

NICENOVEMBER_IMAGE=$IMAGE NICENOVEMBER_DIR=$DIR "$DIR/update.sh"
echo "Done. Logs: podman logs -f nicenovember"
