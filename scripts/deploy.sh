#!/bin/sh
set -eu
[ "$#" -eq 1 ] || { echo 'Usage: scripts/deploy.sh user@raspberrypi.local' >&2; exit 1; }
cd "$(dirname "$0")/.."
[ -f dist/pi-dashboard ] || { echo 'Run scripts/build-aarch64.sh first.' >&2; exit 1; }
scp -r dist "$1:~/pi-dashboard-release"
ssh -t "$1" 'sudo ~/pi-dashboard-release/install.sh'
