#!/usr/bin/env bash
# Installs this checkout's glide-phone over the packaged one and restarts the daemon.
# (The glide package still ships /usr/bin/glide-phone; pacman will overwrite it again on the
# next glide upgrade until that package drops it.)
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -p glide-phone
sudo install -m755 target/release/glide-phone /usr/bin/glide-phone
# Stop every running daemon (autostart and D-Bus activation may have started two).
pkill -x glide-phone || true
sleep 1
# Start it again the way the session does; D-Bus activation also starts it on demand.
setsid -f /usr/bin/glide-phone --daemon >/dev/null 2>&1
sleep 2
pgrep -ax glide-phone
