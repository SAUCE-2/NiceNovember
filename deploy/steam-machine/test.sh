#!/usr/bin/env bash
# Runs install.sh against fake podman/systemctl/loginctl/sudo, in a throwaway HOME, and checks what
# it did. Used by CI; run it locally with deploy/steam-machine/test.sh.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
export HOME=$T/home STATE=$T/state USER=deck XDG_CONFIG_HOME=$T/home/.config
mkdir -p "$HOME" "$STATE" "$T/bin"

# Fake podman, systemctl and sudo just record their calls; loginctl says linger is off.
for cmd in podman systemctl sudo; do
  cat >"$T/bin/$cmd" <<SHIM
#!/usr/bin/env bash
echo "$cmd \$*" >>"\$STATE/calls"
SHIM
done
cat >"$T/bin/loginctl" <<'SHIM'
#!/usr/bin/env bash
echo no
SHIM
chmod +x "$T/bin/"*
export PATH=$T/bin:$PATH

fail() { echo "FAIL: $*" >&2; exit 1; }
called() { grep -qx -- "$1" "$STATE/calls" || fail "not called: $1"; }

# 1. First install only creates the env file.
out=$("$HERE/install.sh")
[[ $out == *"Fill in DISCORD_TOKEN"* ]] || fail "first run should ask for the token: $out"
[ -f "$HOME/nicenovember/.env" ] || fail ".env not created"
[ "$(stat -c %a "$HOME/nicenovember/.env")" = 600 ] || fail ".env should be private"
[ ! -f "$STATE/calls" ] || fail "nothing should run before the token is set"

# 2. An empty token is refused.
"$HERE/install.sh" 2>/dev/null && fail "empty token accepted"

# 3. Upgrading an install that used the old update timer: the bot becomes a Quadlet service that
# podman-auto-update keeps on the latest image, and the old timer and script are gone.
units=$XDG_CONFIG_HOME/systemd/user
mkdir -p "$units"
touch "$units/nicenovember-update.service" "$units/nicenovember-update.timer" "$HOME/nicenovember/update.sh"
sed -i 's/^DISCORD_TOKEN=$/DISCORD_TOKEN=x/; s/^JEV_API_KEY=$/JEV_API_KEY=y/' "$HOME/nicenovember/.env"
out=$("$HERE/install.sh")
[[ $out == *"set DASHBOARD_URL"* ]] || fail "should say how to get dashboard links: $out"

quadlet=$XDG_CONFIG_HOME/containers/systemd/nicenovember.container
for line in Image=ghcr.io/sauce-2/nicenovember:latest ContainerName=nicenovember \
  "EnvironmentFile=$HOME/nicenovember/.env" Volume=nicenovember-data:/data PublishPort=8080:8080 \
  AutoUpdate=registry \
  Restart=always WantedBy=default.target; do
  grep -qx -- "$line" "$quadlet" || fail "$quadlet lacks $line"
done
grep -qx "OnCalendar=\*:0/2" "$units/podman-auto-update.timer.d/nicenovember.conf" || fail "update interval"
called "systemctl --user disable --now nicenovember-update.timer"
for old in "$units/nicenovember-update.service" "$units/nicenovember-update.timer" "$HOME/nicenovember/update.sh"; do
  [ ! -e "$old" ] || fail "$old left behind"
done
called "systemctl --user enable --now podman-auto-update.timer"
called "systemctl --user restart nicenovember.service"
called "sudo loginctl enable-linger deck"

# 4. Podman's own generator accepts the unit, where it's installed (it is on CI's Ubuntu).
for q in /usr/libexec/podman/quadlet /usr/lib/podman/quadlet; do
  [ -x "$q" ] || continue
  QUADLET_UNIT_DIRS=${quadlet%/*} "$q" -dryrun -user >"$T/generated" 2>&1 || fail "quadlet: $(cat "$T/generated")"
  grep -q -- "--env-file $HOME/nicenovember/.env" "$T/generated" || fail "quadlet: $(cat "$T/generated")"
  echo "quadlet generator ($q): ok"
done

# 5. Re-installing keeps the filled-in .env, and once DASHBOARD_URL is set stops asking for it.
echo "DASHBOARD_URL=http://deck.local:8080" >>"$HOME/nicenovember/.env"
out=$("$HERE/install.sh")
grep -q '^DISCORD_TOKEN=x$' "$HOME/nicenovember/.env" || fail ".env overwritten"
[[ $out != *"set DASHBOARD_URL"* ]] || fail "asked for DASHBOARD_URL again: $out"

# 6. The dashboard can go on another port.
NICENOVEMBER_PORT=9090 "$HERE/install.sh" >/dev/null
grep -qx "PublishPort=9090:8080" "$quadlet" || fail "dashboard port"

echo "deploy scripts: ok"
