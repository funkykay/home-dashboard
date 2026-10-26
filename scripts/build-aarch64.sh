#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
mkdir -p dist
docker buildx build --platform linux/arm64 --file deploy/Dockerfile.build --target artifact --output type=local,dest=dist .
cp deploy/pi-dashboard.service deploy/pi-dashboard.env scripts/install.sh scripts/update.sh dist/
cp -R examples docs dist/
cp README.md LICENSE dist/
printf '%s\n' 'Release files are in dist/'
