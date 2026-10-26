#!/usr/bin/env bash
set -euo pipefail

readonly REPO="funkykay/home-dashboard"
readonly BINARY="/usr/local/bin/pi-dashboard"
readonly SERVICE="pi-dashboard.service"

if (( EUID != 0 )); then
  echo "Please run as root (sudo)." >&2
  exit 1
fi

if (( $# > 1 )); then
  echo "Usage: $0 [vMAJOR.MINOR.PATCH]" >&2
  exit 2
fi

version="${1:-latest}"

if [[ "$version" != latest && ! "$version" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "Invalid version: $version" >&2
  exit 2
fi

[[ "$(uname -s)" == Linux ]] || { echo "Linux AArch64 required." >&2; exit 1; }

architecture="$(uname -m)"
bits="$(getconf LONG_BIT)"

case "${architecture}:${bits}" in
  aarch64:64)
    target="aarch64-unknown-linux-gnu"
    ;;
  *)
    echo "Unsupported platform: ${architecture} (${bits}-bit)." >&2
    exit 1
    ;;
esac

readonly TARGET="$target"
readonly ASSET="pi-dashboard-${TARGET}.tar.gz"

systemctl cat "$SERVICE" >/dev/null

if [[ "$version" == latest ]]; then
  base_url="https://github.com/${REPO}/releases/latest/download"
else
  base_url="https://github.com/${REPO}/releases/download/${version}"
fi

workdir="$(mktemp -d)"
stage=""

trap 'if [[ -n "$stage" ]]; then rm -f -- "$stage"; fi; rm -rf -- "$workdir"' EXIT

curl --fail --location --silent --show-error --retry 3 \
  --output "${workdir}/${ASSET}" \
  "${base_url}/${ASSET}"

curl --fail --location --silent --show-error --retry 3 \
  --output "${workdir}/${ASSET}.sha256" \
  "${base_url}/${ASSET}.sha256"

if ! (
  cd "$workdir"
  sha256sum --check --status "${ASSET}.sha256"
); then
  echo "Release checksum validation failed." >&2
  exit 1
fi

tar -xzf "${workdir}/${ASSET}" \
  -C "$workdir" \
  -- pi-dashboard

test -s "${workdir}/pi-dashboard"

replace_binary() {
  stage="$(mktemp "${BINARY}.new.XXXXXX")"

  install -m 0755 "$1" "$stage"
  mv -f -- "$stage" "$BINARY"

  stage=""
}

restart_and_check() {
  systemctl restart "$SERVICE" || return 1
  sleep 2
  systemctl is-active --quiet "$SERVICE"
}

if [[ -f "$BINARY" ]]; then
  cp -p -- "$BINARY" "${workdir}/previous"
fi

replace_binary "${workdir}/pi-dashboard"

if ! restart_and_check; then
  echo "Service failed to start or stopped immediately; rolling back." >&2

  if [[ -f "${workdir}/previous" ]]; then
    replace_binary "${workdir}/previous"

    if ! restart_and_check; then
      echo "Rollback restored the previous binary, but the service is not running." >&2
    fi
  else
    systemctl stop "$SERVICE" || true
    rm -f -- "$BINARY"
  fi

  exit 1
fi

printf 'Installed %s (%s) and started %s.\n' \
  "$version" "$TARGET" "$SERVICE"
