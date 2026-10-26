#!/bin/sh
set -eu
[ "$(id -u)" -eq 0 ] || { echo 'Run with sudo on the Raspberry Pi.' >&2; exit 1; }
[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = aarch64 ] || { echo 'Linux AArch64 required.' >&2; exit 1; }
cd "$(dirname "$0")"
[ -f pi-dashboard ] && [ -f pi-dashboard.sha256 ] && [ -f pi-dashboard.service ] && [ -f pi-dashboard.env ] || { echo 'Run the install.sh shipped in dist/, alongside the release files.' >&2; exit 1; }
sha256sum -c pi-dashboard.sha256
apt-get update
alsa_package=libasound2
if apt-cache show libasound2t64 >/dev/null 2>&1; then alsa_package=libasound2t64; fi
apt-get install -y --no-install-recommends "$alsa_package" alsa-utils fonts-dejavu-core ca-certificates
for group in audio video render tty; do getent group "$group" >/dev/null || groupadd --system "$group"; done
if ! id dashboard >/dev/null 2>&1; then useradd --system --user-group --no-create-home --shell /usr/sbin/nologin dashboard; fi
usermod -a -G audio,video,render,tty dashboard
systemctl stop pi-dashboard.service 2>/dev/null || true
install -m 0755 pi-dashboard /usr/local/bin/pi-dashboard
install -m 0644 pi-dashboard.service /etc/systemd/system/pi-dashboard.service
if [ ! -f /etc/pi-dashboard.env ]; then install -m 0644 pi-dashboard.env /etc/pi-dashboard.env; fi
cat > /etc/udev/rules.d/70-pi-dashboard.rules <<'RULES'
SUBSYSTEM=="graphics", KERNEL=="fb[0-9]*", GROUP="video", MODE="0660"
SUBSYSTEM=="drm", KERNEL=="card[0-9]*", GROUP="video", MODE="0660"
SUBSYSTEM=="tty", KERNEL=="tty1", GROUP="dashboard", MODE="0660"
RULES
udevadm control --reload-rules
udevadm trigger --subsystem-match=graphics
udevadm trigger --subsystem-match=drm
udevadm trigger --subsystem-match=tty --sysname-match=tty1
udevadm settle
systemctl disable --now getty@tty1.service
systemctl daemon-reload
systemctl enable pi-dashboard.service
printf '%s\n' 'Installed and enabled. Configure 1080p HDMI and DASHBOARD_ALSA as described in README.md, then reboot or run: sudo systemctl start pi-dashboard'
