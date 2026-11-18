#!/usr/bin/env bash
# Installs Wispr Flow for Linux in a clean container of each distro, as a normal sudo user,
# from zsh (or via `curl | bash`), then launches it under Xvfb and checks the helper handshake.
#
#   tests/distro-test.sh [image ...]          default: all images below
#   WISPR_LINUX_DMG=~/Downloads/Flow.dmg tests/distro-test.sh   reuse a local .dmg instead of downloading it
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
IMAGES=("$@")
[ ${#IMAGES[@]} -gt 0 ] || IMAGES=(debian:12 debian:13 ubuntu:22.04 ubuntu:24.04 archlinux:latest fedora:latest opensuse/tumbleweed opensuse/leap:15.6)

DMG_MOUNT=()
if [ -n "${WISPR_LINUX_DMG:-}" ]; then
  DMG_MOUNT=(-v "$(realpath "$WISPR_LINUX_DMG"):/flow.dmg:ro" -e WISPR_LINUX_DMG=/flow.dmg)
fi

# Inside the container: only the test scaffolding (sudo, zsh, Xvfb, a user) is set up here.
# Everything the app needs must be installed by install.sh itself.
read -r -d '' INNER <<'EOS' || true
set -eu
if command -v apt-get >/dev/null; then
  apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq sudo zsh xvfb procps >/dev/null
elif command -v dnf >/dev/null; then
  dnf install -y -q sudo zsh xorg-x11-server-Xvfb procps-ng >/dev/null
elif command -v pacman >/dev/null; then
  pacman -Syu --noconfirm --needed sudo zsh xorg-server-xvfb procps-ng >/dev/null
elif command -v zypper >/dev/null; then
  zypper --non-interactive --quiet install sudo zsh xvfb-run xorg-x11-server-Xvfb procps shadow >/dev/null
fi
useradd -m -s /usr/bin/zsh tester 2>/dev/null || useradd -m tester
echo 'tester ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/tester
DMG_ENV=""
[ -n "${WISPR_LINUX_DMG:-}" ] && DMG_ENV="WISPR_LINUX_DMG=$WISPR_LINUX_DMG"
if [ "${VIA_CURL:-}" = 1 ]; then
  # the one-liner from the README, run from zsh
  su - tester -c "zsh -c '$DMG_ENV curl -fsSL file:///repo/install.sh | $DMG_ENV bash'" 2>&1 | grep -E '==>|error' || true
else
  su - tester -c "zsh -c 'cd /repo && $DMG_ENV ./install.sh'" 2>&1 | grep -E '==>|error' || true
fi
su - tester -c 'test -x ~/.local/bin/wispr-flow' || { echo "RESULT: FAIL install"; exit 1; }
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
  if docker run --rm --shm-size=1g -v "$REPO:/repo:ro" "${DMG_MOUNT[@]}" -e VIA_CURL=$via_curl "$img" bash -c "$INNER"; then
    pass=$((pass + 1))
  fi
done
echo "=== $pass/${#IMAGES[@]} distros passed"
[ "$pass" -eq "${#IMAGES[@]}" ]
