#!/usr/bin/env bash
# Wispr Flow for Linux: downloads the official macOS build from wisprflow.ai, converts it
# into a Linux Electron app and plugs in a native Linux helper. Nothing from Wispr is
# redistributed by this repository; everything is fetched and patched on your machine.
#
#   ./install.sh            install or update to the latest Wispr Flow release
#   ./install.sh uninstall  remove it (keeps your Wispr settings in ~/.config/Wispr Flow)
#   curl -fsSL https://raw.githubusercontent.com/cufelix/wispr-flow-linux/main/install.sh | bash
#
# Works from any login shell (bash, zsh, fish...): the script itself always runs in bash.
# Env: WISPR_LINUX_DMG=/path/to/Flow.dmg reuses a downloaded .dmg; WISPR_LINUX_NO_DEPS=1 skips package installs.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd || pwd)"
REPO_TARBALL="https://github.com/cufelix/wispr-flow-linux/archive/refs/heads/main.tar.gz"
PREFIX="${WISPR_LINUX_PREFIX:-$HOME/.local/share/wispr-flow-linux}"
BIN_DIR="$HOME/.local/bin"
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
SQLITE3_VERSION=5.1.7
HELPER_RELEASE_URL="https://github.com/cufelix/wispr-flow-linux/releases/latest/download"

say() { printf '\033[1;35m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# Only ever delete a directory this script created (it carries a WISPR_VERSION marker).
safe_rm_prefix() {
  case "$PREFIX" in /|"$HOME"|"$HOME"/) die "refusing to delete $PREFIX" ;; esac
  if [ -e "$PREFIX" ] && [ ! -f "$PREFIX/WISPR_VERSION" ]; then
    die "$PREFIX exists but was not created by this installer; refusing to delete it"
  fi
  rm -rf "$PREFIX"
}

if [ "${1:-}" = uninstall ]; then
  pkill -x wispr-flow 2>/dev/null || true
  safe_rm_prefix
  rm -f "$BIN_DIR/wispr-flow" "$APPS_DIR/wispr-flow.desktop" "$AUTOSTART_DIR/wispr-flow.desktop"
  say "Uninstalled. Your Wispr settings remain in ~/.config/Wispr Flow."
  exit 0
fi

case "$(uname -m)" in
  x86_64) ARCH=x64; RUST_TARGET=x86_64-unknown-linux-musl ;;
  aarch64|arm64) ARCH=arm64; RUST_TARGET=aarch64-unknown-linux-musl ;;
  *) die "unsupported CPU architecture $(uname -m) (x86_64 and arm64 only)" ;;
esac

SUDO=""
[ "$(id -u)" -eq 0 ] || SUDO=sudo

# Tools to unpack the .dmg plus the shared libraries Electron needs (minimal installs lack some).
install_deps() {
  [ -z "${WISPR_LINUX_NO_DEPS:-}" ] || { say "WISPR_LINUX_NO_DEPS set, not installing packages"; return; }
  if [ -n "$SUDO" ] && ! command -v sudo >/dev/null; then
    die "missing packages and no sudo; install xz, curl and Electron's libraries (nss, gtk3, alsa, libgbm) as root"
  fi
  say "Installing required packages (you may be asked for your password)"
  if command -v apt-get >/dev/null; then
    $SUDO apt-get update -qq
    # Debian 13 / Ubuntu 24.04 renamed several libs to *t64 during the 64-bit time_t transition
    local pkgs=() p
    for p in xz-utils curl ca-certificates tar libnss3 libgtk-3-0 libgbm1 libasound2 libxss1 libxtst6 \
             libnotify4 libsecret-1-0 libdrm2 libxkbcommon0 libatk-bridge2.0-0 libcups2 xdg-utils util-linux; do
      if apt-cache show "${p}t64" >/dev/null 2>&1; then pkgs+=("${p}t64"); else pkgs+=("$p"); fi
    done
    $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${pkgs[@]}" >/dev/null
  elif command -v dnf >/dev/null; then
    $SUDO dnf install -y -q xz curl tar nss gtk3 mesa-libgbm alsa-lib libXScrnSaver libXtst libnotify libsecret \
      libdrm libxkbcommon at-spi2-atk cups-libs xdg-utils util-linux
  elif command -v pacman >/dev/null; then
    local arch_pkgs=(xz curl tar nss gtk3 mesa alsa-lib libxss libxtst libnotify libsecret libdrm
                     libxkbcommon at-spi2-core libcups xdg-utils util-linux)
    # a stale package database fails with 404s; Arch only supports full upgrades, so then use -Syu
    $SUDO pacman -S --needed --noconfirm "${arch_pkgs[@]}" 2>/dev/null \
      || $SUDO pacman -Syu --needed --noconfirm "${arch_pkgs[@]}"
  elif command -v zypper >/dev/null; then
    $SUDO zypper --non-interactive --quiet install xz curl tar mozilla-nss libgtk-3-0 libgbm1 alsa \
      libXss1 libXtst6 libnotify4 libsecret-1-0 libdrm2 libxkbcommon0 libatk-bridge-2_0-0 libcups2 xdg-utils util-linux
  else
    die "unknown package manager; install xz, curl, tar and Electron's libraries (nss, gtk3, alsa, libgbm), then rerun"
  fi
  DEPS_DONE=1
}

DEPS_DONE=""
if ! command -v curl >/dev/null || ! command -v tar >/dev/null || ! command -v xz >/dev/null; then
  install_deps
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Official static 7-Zip: distro p7zip (e.g. 16.02 on Ubuntu 22.04) cannot open current macOS .dmg files.
SEVENZIP_VERSION=2606
case "$ARCH" in
  x64) SEVENZIP_SHA256=2b539cf067b2ad48e1baa5093dbaa8818b73542d1222a0ffde9dc5f2922ea62e ;;
  arm64) SEVENZIP_SHA256=edcc408aacce77da5223728f2eceb5e9f6dc713b8a9df934ad21e2a7f7560627 ;;
esac
curl -fsSL -o "$WORK/7z.tar.xz" \
  "https://github.com/ip7z/7zip/releases/download/${SEVENZIP_VERSION:0:2}.${SEVENZIP_VERSION:2}/7z$SEVENZIP_VERSION-linux-$ARCH.tar.xz"
echo "$SEVENZIP_SHA256  $WORK/7z.tar.xz" | sha256sum -c --quiet || die "7-Zip checksum mismatch"
mkdir -p "$WORK/7z" && tar -xJf "$WORK/7z.tar.xz" -C "$WORK/7z" 7zzs
SEVENZIP="$WORK/7z/7zzs"

# Running via `curl | bash`: fetch the rest of this repository (patcher/, helper/).
if [ ! -f "$REPO_DIR/patcher/prepare.js" ]; then
  say "Fetching wispr-flow-linux"
  mkdir -p "$WORK/repo"
  curl -fsSL "$REPO_TARBALL" | tar -xz -C "$WORK/repo" --strip-components=1
  REPO_DIR="$WORK/repo"
fi

if [ -n "${WISPR_LINUX_DMG:-}" ]; then
  [ -f "$WISPR_LINUX_DMG" ] || die "WISPR_LINUX_DMG=$WISPR_LINUX_DMG not found"
  say "Using local Wispr Flow .dmg"
  ln -s "$(cd "$(dirname "$WISPR_LINUX_DMG")" && pwd)/$(basename "$WISPR_LINUX_DMG")" "$WORK/flow.dmg"
else
  say "Finding the latest Wispr Flow release"
  DMG_URL=$(curl -fsSIL -o /dev/null -w '%{url_effective}' https://dl.wisprflow.ai/mac-apple/latest)
  say "Downloading $(basename "$DMG_URL") (macOS build, ~300 MB)"
  curl -fL --progress-bar -o "$WORK/flow.dmg" "$DMG_URL"
fi

say "Unpacking the .dmg"
"$SEVENZIP" x -y -o"$WORK/dmg" "$WORK/flow.dmg" '*/Wispr Flow.app/Contents/Resources/*' \
  '*/Wispr Flow.app/Contents/Info.plist' \
  '*/Wispr Flow.app/Contents/Frameworks/Electron Framework.framework/Versions/A/Resources/Info.plist' >/dev/null
CONTENTS=$(find "$WORK/dmg" -maxdepth 3 -type d -path '*/Wispr Flow.app/Contents' | head -1)
[ -n "$CONTENTS" ] || die "unexpected .dmg layout"
VERSION=$(grep -A1 CFBundleShortVersionString "$CONTENTS/Info.plist" | grep -oE '[0-9][0-9.]+' | head -1)
[ -n "$VERSION" ] || die "could not detect the Wispr Flow version"
ELECTRON_VERSION=$(grep -A1 CFBundleVersion "$CONTENTS/Frameworks/Electron Framework.framework/Versions/A/Resources/Info.plist" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
[ -n "$ELECTRON_VERSION" ] || die "could not detect the Electron version"

say "Downloading Electron $ELECTRON_VERSION for Linux ($ARCH)"
curl -fL --progress-bar -o "$WORK/electron.zip" \
  "https://github.com/electron/electron/releases/download/v$ELECTRON_VERSION/electron-v$ELECTRON_VERSION-linux-$ARCH.zip"
"$SEVENZIP" x -y -o"$WORK/electron" "$WORK/electron.zip" >/dev/null
mv "$WORK/electron/electron" "$WORK/electron/wispr-flow" # a non-"electron" name makes app.isPackaged true

# Electron's shared libraries: install them if this system lacks any.
if ldd "$WORK/electron/wispr-flow" 2>/dev/null | grep -q "not found"; then
  [ -n "$DEPS_DONE" ] || install_deps
  MISSING=$(ldd "$WORK/electron/wispr-flow" | grep "not found" || true)
  [ -z "$MISSING" ] || die "Electron still misses libraries: $MISSING"
fi

say "Converting the app for Linux"
ELECTRON_RUN_AS_NODE=1 "$WORK/electron/wispr-flow" "$REPO_DIR/patcher/prepare.js" \
  "$CONTENTS/Resources" "$WORK/electron/resources"

say "Fetching the Linux SQLite module"
curl -fsSL "https://github.com/TryGhost/node-sqlite3/releases/download/v$SQLITE3_VERSION/sqlite3-v$SQLITE3_VERSION-napi-v6-linux-$ARCH.tar.gz" \
  | tar -xz -C "$WORK"
cp "$WORK/build/Release/node_sqlite3.node" \
  "$WORK/electron/resources/app/.webpack/main/native_modules/build/Release/node_sqlite3.node"

say "Installing the Linux helper"
HELPER="$WORK/electron/wispr-flow-linux-helper"
if command -v cargo >/dev/null; then
  (cd "$REPO_DIR" && cargo build --release -p wispr-flow-linux-helper)
  cp "$REPO_DIR/target/release/wispr-flow-linux-helper" "$HELPER"
else
  NAME="wispr-flow-linux-helper-$RUST_TARGET"
  curl -fL --progress-bar -o "$HELPER" "$HELPER_RELEASE_URL/$NAME"
  curl -fsSL -o "$WORK/$NAME.sha256" "$HELPER_RELEASE_URL/$NAME.sha256"
  (cd "$WORK" && sed "s#  .*#  electron/wispr-flow-linux-helper#" "$NAME.sha256" | sha256sum -c --quiet) \
    || die "helper checksum mismatch"
fi
chmod +x "$HELPER"

echo "$VERSION" > "$WORK/electron/WISPR_VERSION"
mkdir -p "$(dirname "$PREFIX")" "$BIN_DIR" "$APPS_DIR" "$AUTOSTART_DIR"
# copy next to the target first (may cross filesystems), then swap, so a failure never leaves you without an app
rm -rf "$PREFIX.new"
cp -a "$WORK/electron" "$PREFIX.new"
pkill -x wispr-flow 2>/dev/null || true
safe_rm_prefix
mv "$PREFIX.new" "$PREFIX"

cat > "$BIN_DIR/wispr-flow" <<LAUNCHER
#!/usr/bin/env bash
export WISPR_LINUX_HELPER='$PREFIX/wispr-flow-linux-helper'
SANDBOX=()
# Chromium's sandbox needs unprivileged user namespaces (or a setuid chrome-sandbox).
if ! unshare -Ur true 2>/dev/null; then
  echo "wispr-flow: user namespaces unavailable, running without the Chromium sandbox" >&2
  SANDBOX=(--no-sandbox)
fi
exec '$PREFIX/wispr-flow' "\${SANDBOX[@]}" "\$@"
LAUNCHER
chmod +x "$BIN_DIR/wispr-flow"

desktop_entry() {
  local exec_path="${BIN_DIR//%/%%}" # '%' is a field code in .desktop files
  cat <<DESKTOP
[Desktop Entry]
Type=Application
Name=Wispr Flow
Comment=Voice dictation in every app
Exec="$exec_path/wispr-flow" $1
Icon=$PREFIX/resources/wispr-flow.png
Terminal=false
Categories=Utility;Accessibility;
StartupWMClass=Wispr Flow
MimeType=x-scheme-handler/wispr-flow;
DESKTOP
}
desktop_entry %U > "$APPS_DIR/wispr-flow.desktop"
desktop_entry "" > "$AUTOSTART_DIR/wispr-flow.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$APPS_DIR" 2>/dev/null || true
command -v xdg-mime >/dev/null && xdg-mime default wispr-flow.desktop x-scheme-handler/wispr-flow || true

say "Wispr Flow $VERSION installed. Start it from your app menu or run: wispr-flow"
if [ "${XDG_SESSION_TYPE:-}" = wayland ] && ! id -nG | grep -qw input; then
  echo "    Wayland: allow global shortcuts and typing with:"
  echo "    sudo usermod -aG input \$USER && echo 'KERNEL==\"uinput\", GROUP=\"input\", MODE=\"0660\"' | sudo tee /etc/udev/rules.d/70-wispr-flow-uinput.rules && sudo udevadm control --reload && sudo udevadm trigger"
  echo "    then log out and back in."
fi
