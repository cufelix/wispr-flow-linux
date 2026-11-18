#!/usr/bin/env bash
# Wispr Flow for Linux: downloads the official macOS build from wisprflow.ai, converts it
# into a Linux Electron app and plugs in a native Linux helper. Nothing from Wispr is
# redistributed by this repository; everything is fetched and patched on your machine.
#
#   ./install.sh            install or update to the latest Wispr Flow release
#   ./install.sh uninstall  remove it (keeps your Wispr settings in ~/.config/Wispr Flow)
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
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

SEVENZIP=$(command -v 7z || command -v 7zz || command -v 7za || true)
[ -n "$SEVENZIP" ] || die "7-Zip is required to open the macOS .dmg. Install it:
  Debian/Ubuntu: sudo apt install p7zip-full    Fedora: sudo dnf install p7zip-plugins
  Arch: sudo pacman -S 7zip                     openSUSE: sudo zypper install 7zip"
for tool in curl tar; do command -v "$tool" >/dev/null || die "$tool is required"; done

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

say "Finding the latest Wispr Flow release"
DMG_URL=$(curl -fsSIL -o /dev/null -w '%{url_effective}' https://dl.wisprflow.ai/mac-apple/latest)
VERSION=$(basename "$DMG_URL" .dmg | sed 's/^Flow-v//')
say "Downloading Wispr Flow $VERSION (macOS build, ~300 MB)"
curl -fL --progress-bar -o "$WORK/flow.dmg" "$DMG_URL"

say "Unpacking the .dmg"
"$SEVENZIP" x -y -o"$WORK/dmg" "$WORK/flow.dmg" '*/Wispr Flow.app/Contents/Resources/*' \
  '*/Wispr Flow.app/Contents/Frameworks/Electron Framework.framework/Versions/A/Resources/Info.plist' >/dev/null
CONTENTS=$(find "$WORK/dmg" -maxdepth 3 -type d -path '*/Wispr Flow.app/Contents' | head -1)
[ -n "$CONTENTS" ] || die "unexpected .dmg layout"
ELECTRON_VERSION=$(grep -A1 CFBundleVersion "$CONTENTS/Frameworks/Electron Framework.framework/Versions/A/Resources/Info.plist" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
[ -n "$ELECTRON_VERSION" ] || die "could not detect the Electron version"

say "Downloading Electron $ELECTRON_VERSION for Linux ($ARCH)"
curl -fL --progress-bar -o "$WORK/electron.zip" \
  "https://github.com/electron/electron/releases/download/v$ELECTRON_VERSION/electron-v$ELECTRON_VERSION-linux-$ARCH.zip"
"$SEVENZIP" x -y -o"$WORK/electron" "$WORK/electron.zip" >/dev/null
mv "$WORK/electron/electron" "$WORK/electron/wispr-flow" # a non-"electron" name makes app.isPackaged true

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
