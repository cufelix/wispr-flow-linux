#!/usr/bin/env bash
# Installs Wispr Flow for Linux in a clean container of each distro, as a normal sudo user,
# from bash or zsh (and via `curl | bash`), then launches it under Xvfb and checks the helper handshake.
#
#   tests/distro-test.sh [image ...]          default: all images below
#   WISPR_LINUX_DMG=~/Downloads/Flow.dmg tests/distro-test.sh   reuse a local .dmg instead of downloading it
#   TEST_UPDATE=1 tests/distro-test.sh ubuntu:24.04             also check the background auto-update (downloads ~300 MB)
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
IMAGES=("$@")
[ ${#IMAGES[@]} -gt 0 ] || IMAGES=(debian:12 debian:13 ubuntu:22.04 ubuntu:24.04 archlinux:latest fedora:latest opensuse/tumbleweed opensuse/leap:15.6 linuxmintd/mint22-amd64)

DOCKER_ARGS=()
# TEST_DNS=<ip> if the default resolver in containers is flaky on your network
[ -z "${TEST_DNS:-}" ] || DOCKER_ARGS+=(--dns "$TEST_DNS")
DMG_MOUNT=()
if [ -n "${WISPR_LINUX_DMG:-}" ]; then
  DMG_MOUNT=(-v "$(realpath "$WISPR_LINUX_DMG"):/flow.dmg:ro" -e WISPR_LINUX_DMG=/flow.dmg)
fi

# Inside the container: only the test scaffolding (sudo, zsh, Xvfb, a user) is set up here.
# Everything the app needs must be installed by install.sh itself.
read -r -d '' INNER <<'EOS' || true
set -eu
if command -v apt-get >/dev/null; then
  apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq sudo zsh xvfb procps curl ca-certificates >/dev/null
elif command -v dnf >/dev/null; then
  dnf install -y -q curl sudo zsh xorg-x11-server-Xvfb procps-ng >/dev/null
elif command -v pacman >/dev/null; then
  pacman -Syu --noconfirm --needed curl sudo zsh xorg-server-xvfb procps-ng >/dev/null
elif command -v zypper >/dev/null; then
  zypper --non-interactive --quiet install curl sudo zsh xvfb-run xorg-x11-server-Xvfb procps shadow >/dev/null
fi
SH=${TEST_SHELL:-zsh}
command -v "$SH" >/dev/null || { echo "RESULT: FAIL setup ($SH not installed, network?)"; exit 1; }
useradd -m -s "$(command -v $SH)" tester
echo 'tester ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/tester
DMG_ENV=""
[ -n "${WISPR_LINUX_DMG:-}" ] && DMG_ENV="WISPR_LINUX_DMG=$WISPR_LINUX_DMG"
if [ "${VIA_CURL:-}" = 1 ]; then
  # the one-liner from the README, run from zsh
  su - tester -c "$SH -c '$DMG_ENV curl -fsSL file:///repo/install.sh | $DMG_ENV bash'" 2>&1 | grep -E '==>|error|not found|denied' || true
else
  su - tester -c "$SH -c 'cd /repo && $DMG_ENV ./install.sh'" 2>&1 | grep -E '==>|error|not found|denied' || true
fi
su - tester -c 'test -x ~/.local/bin/wispr-flow' || { echo "RESULT: FAIL install"; exit 1; }
if [ "${TEST_UPDATE:-}" = 1 ]; then
  # pretend an older version is installed: the background updater must stage the latest one
  P=/home/tester/.local/share/wispr-flow-linux
  echo 0.0.1 > $P/WISPR_VERSION
  su - tester -c "bash $P/updater/install.sh auto-update"
  test -f $P.staged/WISPR_VERSION || { echo "RESULT: FAIL auto-update"; tail -20 /home/tester/.cache/wispr-flow-linux/update.log; exit 1; }
  echo "==> staged $(cat $P.staged/WISPR_VERSION)"
fi
su - tester -c '(Xvfb :99 -screen 0 1920x1080x24 >/dev/null 2>&1 &); sleep 2; DISPLAY=:99 timeout 60 ~/.local/bin/wispr-flow > /tmp/run.log 2>&1; true'
grep -q "Helper service is ready: true" /tmp/run.log && grep -q "migrations have been successfully executed" /tmp/run.log \
  && echo "RESULT: PASS ($(cat /home/tester/.local/share/wispr-flow-linux/WISPR_VERSION))" \
  || { echo "RESULT: FAIL launch"; grep -iE "error|fatal|helper" /tmp/run.log | head -20; exit 1; }
EOS

pass=0
for img in "${IMAGES[@]}"; do
  echo "=== $img"
  via_curl=0
  [ "$img" = "ubuntu:24.04" ] && via_curl=1
  # half the matrix runs the installer from bash, half from zsh
  case "$img" in debian:12|ubuntu:22.04|archlinux:*|opensuse/leap:*) shell=bash ;; *) shell=zsh ;; esac
  echo "    (from $shell$([ $via_curl = 1 ] && echo ', piped one-liner'))"
  if docker run --rm --shm-size=1g "${DOCKER_ARGS[@]}" -v "$REPO:/repo:ro" "${DMG_MOUNT[@]}" -e VIA_CURL=$via_curl -e TEST_SHELL=$shell -e TEST_UPDATE="${TEST_UPDATE:-}" "$img" bash -c "$INNER"; then
    pass=$((pass + 1))
  fi
done
echo "=== $pass/${#IMAGES[@]} distros passed"
[ "$pass" -eq "${#IMAGES[@]}" ]
