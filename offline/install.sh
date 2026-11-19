#!/usr/bin/env bash
# Build, install to ~/.local/bin, fetch model, enable autostart, start now.
# Usage: ./install.sh [extra wisperflow-linux args, e.g. --lang cs --key Super_R]
#    or: curl -fsSL https://raw.githubusercontent.com/cufelix/wispr-flow-linux/main/offline/install.sh | bash -s -- --lang cs
set -euo pipefail
# Under `curl | bash` the script itself is stdin: every command below that could read stdin gets </dev/null,
# or it would swallow the rest of the script.

# Use the local checkout only when run as a file; piped (curl | bash) fetches the repo.
if [ -f "${BASH_SOURCE[0]:-}" ] && [ -f "$(dirname "${BASH_SOURCE[0]}")/Cargo.toml" ]; then
  cd "$(dirname "${BASH_SOURCE[0]}")/.."
else
  WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
  curl -fsSL https://github.com/cufelix/wispr-flow-linux/archive/refs/heads/main.tar.gz | tar -xz -C "$WORK" --strip-components=1
  cd "$WORK"
fi

if command -v apt-get >/dev/null; then
  dpkg -s build-essential pkg-config xdotool cmake clang libclang-dev libasound2-dev libvulkan-dev glslc mesa-vulkan-drivers >/dev/null 2>&1 ||
    { sudo apt-get update -qq </dev/null; sudo apt-get install -y build-essential pkg-config xdotool cmake clang libclang-dev libasound2-dev libvulkan-dev glslc mesa-vulkan-drivers curl </dev/null; }
elif command -v dnf >/dev/null; then
  sudo dnf install -y gcc gcc-c++ make pkgconf-pkg-config xdotool cmake clang clang-devel alsa-lib-devel vulkan-loader-devel glslc mesa-vulkan-drivers curl </dev/null
elif command -v pacman >/dev/null; then
  sudo pacman -S --needed --noconfirm base-devel xdotool cmake clang alsa-lib vulkan-icd-loader vulkan-headers shaderc curl </dev/null
else
  echo "Unknown package manager: install a C/C++ toolchain, pkg-config, xdotool, cmake, clang, ALSA and Vulkan dev headers, glslc." >&2
fi

if ! command -v cargo >/dev/null; then
  [ -x "$HOME/.cargo/bin/cargo" ] || curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
  export PATH="$HOME/.cargo/bin:$PATH"
fi

cargo build --release -p wisperflow-offline
install -Dm755 target/release/wisperflow-offline ~/.local/bin/wisperflow-linux

MODEL=~/.local/share/wisperflow-linux/models/ggml-large-v3-turbo-q5_0.bin
if [ ! -f "$MODEL" ]; then
  mkdir -p "$(dirname "$MODEL")"
  curl -fL -o "$MODEL.part" https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin
  mv "$MODEL.part" "$MODEL"
fi

mkdir -p ~/.config/autostart
cat > ~/.config/autostart/wisperflow-linux.desktop <<DESKTOP
[Desktop Entry]
Type=Application
Name=Wisperflow Linux
Comment=Hold Right Ctrl to dictate
Exec=sh -c 'exec $HOME/.local/bin/wisperflow-linux $* >> $HOME/.local/share/wisperflow-linux/log.txt 2>&1'
X-GNOME-Autostart-enabled=true
DESKTOP

pkill -x wisperflow-linu || true
nohup ~/.local/bin/wisperflow-linux "$@" >> ~/.local/share/wisperflow-linux/log.txt 2>&1 </dev/null &
echo "Running. Hold Right Ctrl, speak, release. Log: ~/.local/share/wisperflow-linux/log.txt"
