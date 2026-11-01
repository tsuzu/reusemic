#!/bin/sh
# Removes what install.sh installed. The model weights are kept unless --all is given.
set -eu
BIN_DIR=${BIN_DIR:-/usr/local/bin}
sudo rm -rf /Library/Audio/Plug-Ins/HAL/ReuseMic.driver
sudo rm -f "$BIN_DIR/reusemic"
if [ "${1:-}" = "--all" ]; then
    rm -rf "$HOME/Library/Application Support/reusemic"
fi
sudo killall -9 coreaudiod || true
echo "Uninstalled."
