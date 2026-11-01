#!/bin/sh
# Installs reusemic from a release archive:
#   - ReuseMic.driver -> /Library/Audio/Plug-Ins/HAL (needs sudo), then restarts coreaudiod
#   - reusemic        -> /usr/local/bin (needs sudo)
#   - model weights   -> ~/Library/Application Support/reusemic (downloaded from Hugging Face)
set -eu

cd "$(dirname "$0")"
HAL_DIR=/Library/Audio/Plug-Ins/HAL
BIN_DIR=${BIN_DIR:-/usr/local/bin}
MODEL_DIR="$HOME/Library/Application Support/reusemic"
MODEL="$MODEL_DIR/Real-time_RE-USE.safetensors"
MODEL_URL=https://huggingface.co/nvidia/Real-time_RE-USE/resolve/main/model.safetensors

# Files from a downloaded archive carry the quarantine flag; coreaudiod and Gatekeeper refuse them.
xattr -dr com.apple.quarantine ReuseMic.driver reusemic 2>/dev/null || true

echo "Installing driver to $HAL_DIR (sudo)"
sudo rm -rf "$HAL_DIR/ReuseMic.driver"
sudo cp -R ReuseMic.driver "$HAL_DIR/"
sudo chown -R root:wheel "$HAL_DIR/ReuseMic.driver"

echo "Installing reusemic to $BIN_DIR (sudo)"
sudo mkdir -p "$BIN_DIR"
sudo install -m 755 reusemic "$BIN_DIR/reusemic"

if [ ! -f "$MODEL" ]; then
    echo "Downloading model weights (NVIDIA One-Way Noncommercial License) to $MODEL_DIR"
    mkdir -p "$MODEL_DIR"
    curl -fL --progress-bar -o "$MODEL.part" "$MODEL_URL"
    mv "$MODEL.part" "$MODEL"
fi

echo "Restarting coreaudiod"
sudo killall -9 coreaudiod || true

echo "Done. Run: reusemic --list"
