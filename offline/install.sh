#!/usr/bin/env bash
# Build, install to ~/.local/bin, fetch model, enable autostart, start now.
# Usage: ./install.sh [extra wisperflow-linux args, e.g. --lang cs --key Super_R]
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v xdotool >/dev/null || ! dpkg -s libasound2-dev cmake clang libvulkan-dev glslc >/dev/null 2>&1; then
  sudo apt-get install -y xdotool cmake clang libclang-dev libasound2-dev libvulkan-dev glslc mesa-vulkan-drivers
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
nohup ~/.local/bin/wisperflow-linux "$@" >> ~/.local/share/wisperflow-linux/log.txt 2>&1 &
echo "Running. Hold Right Ctrl, speak, release. Log: ~/.local/share/wisperflow-linux/log.txt"
