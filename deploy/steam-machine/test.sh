#!/usr/bin/env bash
# Runs install.sh and update.sh against fake podman/systemctl/loginctl/sudo, in a throwaway
# HOME, and checks what they did. Used by CI; run it locally with deploy/steam-machine/test.sh.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
export HOME=$T/home STATE=$T/state USER=deck XDG_CONFIG_HOME=$T/home/.config
mkdir -p "$HOME" "$STATE" "$T/bin"
echo aaa >"$STATE/image"

# Fake podman: one image id in $STATE/image, one container in $STATE/container.
cat >"$T/bin/podman" <<'SHIM'
#!/usr/bin/env bash
echo "podman $*" >>"$STATE/calls"
case "$1 $2" in
  "pull -q") ;;
  "image inspect") cat "$STATE/image" ;;
  "container inspect")
    [ -f "$STATE/container" ] || exit 125
    case "$4" in
      "{{.Image}}") echo "sha256:$(cat "$STATE/container")" ;;
      "{{.State.Running}}") cat "$STATE/running" ;;
    esac ;;
  "rm -f") rm -f "$STATE/container" ;;
  "run -d") cp "$STATE/image" "$STATE/container"; echo true >"$STATE/running" ;;
  "image prune") ;;
  *) echo "unexpected: podman $*" >&2; exit 1 ;;
esac
SHIM
# Fake systemctl and sudo just record their calls; loginctl says linger is off.
for cmd in systemctl sudo; do
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
runs() { grep -c '^podman run' "$STATE/calls" || true; }

# 1. First install only creates the env file.
out=$("$HERE/install.sh")
[[ $out == *"Fill in DISCORD_TOKEN"* ]] || fail "first run should ask for the token: $out"
[ -f "$HOME/nicenovember/.env" ] || fail ".env not created"
[ "$(stat -c %a "$HOME/nicenovember/.env")" = 600 ] || fail ".env should be private"
[ ! -f "$STATE/calls" ] || fail "nothing should run before the token is set"

# 2. An empty token is refused.
"$HERE/install.sh" 2>/dev/null && fail "empty token accepted"

# 3. With a token: units written, timer enabled, linger asked for, bot started once.
sed -i 's/^DISCORD_TOKEN=$/DISCORD_TOKEN=x/; s/^JEV_API_KEY=$/JEV_API_KEY=y/' "$HOME/nicenovember/.env"
"$HERE/install.sh" >/dev/null
units=$XDG_CONFIG_HOME/systemd/user
grep -q "ExecStart=$HOME/nicenovember/update.sh" "$units/nicenovember-update.service" || fail "service"
grep -q "OnUnitActiveSec=2min" "$units/nicenovember-update.timer" || fail "timer"
grep -q "systemctl --user enable --now nicenovember-update.timer" "$STATE/calls" || fail "timer not enabled"
grep -q "sudo loginctl enable-linger deck" "$STATE/calls" || fail "linger not enabled"
grep -q -- "--restart=always --env-file $HOME/nicenovember/.env -v nicenovember-data:/data ghcr.io/sauce-2/nicenovember:latest" "$STATE/calls" ||
  fail "container not started as expected"
[ "$(runs)" = 1 ] || fail "expected one start, got $(runs)"

# 4. Same image, still running: the timer leaves it alone (despite the sha256: prefix).
"$HOME/nicenovember/update.sh"
[ "$(runs)" = 1 ] || fail "restarted without a new image"

# 5. A new image: recreated. Stopped: recreated.
echo bbb >"$STATE/image"
"$HOME/nicenovember/update.sh" >/dev/null
[ "$(runs)" = 2 ] || fail "not updated to the new image"
echo false >"$STATE/running"
"$HOME/nicenovember/update.sh" >/dev/null
[ "$(runs)" = 3 ] || fail "stopped container not restarted"

# 6. Re-installing keeps the filled-in .env.
"$HERE/install.sh" >/dev/null
grep -q '^DISCORD_TOKEN=x$' "$HOME/nicenovember/.env" || fail ".env overwritten"

echo "deploy scripts: ok"
