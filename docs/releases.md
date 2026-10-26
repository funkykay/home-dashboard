# Releases

Releases werden wie in `rust-tv` ausschließlich manuell über GitHub Actions
gestartet. Ein Push auf `main` erstellt kein Release. Die Version wird in
`Cargo.toml` gepflegt; der Workflow erstellt daraus den Git-Tag `vMAJOR.MINOR.PATCH`.

## Plattform und Build

Unterstützt wird Raspberry Pi OS Lite **64-Bit**, Bookworm oder neuer:
`aarch64-unknown-linux-gnu`, glibc ab 2.36 und libasound.
Für ARMv7/32-Bit wird kein Artefakt erstellt.

Der Build läuft auf dem GitHub-Runner `ubuntu-24.04-arm` mit dem vorhandenen
`scripts/build-aarch64.sh`. Dieser verwendet das per Digest fixierte
Rust-1.90.0-/Debian-Bookworm-Image aus `deploy/Dockerfile.build`.
Damit verwenden lokale und GitHub-Releases dieselbe Build-Umgebung.
Die Tests laufen nativ unter Linux ARM64 im Container und benötigen keine
Pi-Hardware. HDMI-Bild und -Ton müssen weiterhin am Gerät geprüft werden.

## Release vorbereiten und starten

1. Die gewünschte Version in `Cargo.toml` setzen, beispielsweise `0.1.1`.
2. `Cargo.lock` synchronisieren:

   ```sh
   cargo update --workspace
   ```

3. Die Änderungen committen und nach `main` pushen:

   ```sh
   git add Cargo.toml Cargo.lock
   git commit -m "Prepare release v0.1.1"
   git push origin main
   ```

4. Unter [GitHub Actions](https://github.com/funkykay/home-dashboard/actions/workflows/release.yml)
   den Workflow **Release** öffnen, **Run workflow** auswählen und auf `main` starten.

Eine Version muss beim Start nicht eingegeben werden. Für den ersten Release
kann die vorhandene Version `0.1.0` verwendet werden, sofern `v0.1.0` noch nicht
existiert. Der Workflow muss zuvor auf `main` eingecheckt und gepusht sein.

## Ablauf und Berechtigungen

- **Validate:** Erlaubt ausschließlich `main`, verlangt `Cargo.lock`, liest die
  Paketversion und prüft das Format `MAJOR.MINOR.PATCH`. Vorhandene Tags und Fehler
  beim Abruf der Remote-Tags brechen den Ablauf ab.
- **Build:** Führt Formatierung, Clippy mit Warnungen als Fehler, Tests und
  Release-Build mit `--locked` aus. Ein nicht synchronisiertes Lockfile bricht den
  Build ab. Anschließend werden das Installationsarchiv und seine Prüfsumme erzeugt.
- **Publish:** Prüft die heruntergeladenen Artefakte und veröffentlicht den Tag
  am Commit dieses Workflow-Laufs mit automatisch generierten Release Notes.
  Das Release wird als neuestes Release markiert.

Ein fehlgeschlagener Validate- oder Build-Job erstellt keinen Tag und kein Release.
Release-Läufe werden nicht gleichzeitig ausgeführt; ein laufender Release wird
durch einen weiteren Start nicht abgebrochen.

Nur der Publish-Job erhält `contents: write`; alle anderen Jobs haben ausschließlich
Lesezugriff. Der automatische `GITHUB_TOKEN` genügt. Es sind keine zusätzlichen
Repository-Secrets, kein persönlicher GitHub-Token und kein Zugriff auf das Heimnetz
erforderlich. Downloads aus dem öffentlichen Repository benötigen keine Anmeldung.

## Release-Artefakte

Jedes Release enthält zwei Dateien:

```text
pi-dashboard-aarch64-unknown-linux-gnu.tar.gz
pi-dashboard-aarch64-unknown-linux-gnu.tar.gz.sha256
```

Das Archiv enthält das Binary `pi-dashboard` mit eigener Prüfsumme,
`pi-dashboard.service`, die Beispielkonfiguration `pi-dashboard.env`,
`install.sh`, `update.sh`, `examples/`, `docs/`, README und Lizenz.

## Erstinstallation auf dem Pi

Zuerst Raspberry Pi OS Lite 64-Bit sowie HDMI/KMS gemäß der
[README](../README.md#raspberry-pi-os-lite-einrichten) einrichten.
Dann auf dem Pi das neueste Release herunterladen:

```sh
sudo apt-get update
sudo apt-get install -y curl ca-certificates
mkdir -p ~/pi-dashboard-release
cd ~/pi-dashboard-release

base_url=https://github.com/funkykay/home-dashboard/releases/latest/download
archive=pi-dashboard-aarch64-unknown-linux-gnu.tar.gz
curl -fL --retry 3 -O "$base_url/$archive"
curl -fL --retry 3 -O "$base_url/$archive.sha256"
sha256sum --check "$archive.sha256" && tar -xzf "$archive" && sudo ./install.sh
```

Der Installer richtet Abhängigkeiten, Benutzer, Geräteberechtigungen und systemd ein.
Anschließend `/etc/pi-dashboard.env` anpassen und den Dienst starten oder neu booten,
wie in der README beschrieben.

## Update-Skript installieren

Nach der Erstinstallation das mitgelieferte Skript installieren:

```sh
sudo install -m 0755 ~/pi-dashboard-release/update.sh /usr/local/sbin/home-dashboard-update
```

Für eine bereits vorhandene Installation kann es auch direkt bezogen werden:

```sh
curl -fL --retry 3 \
  https://raw.githubusercontent.com/funkykay/home-dashboard/main/scripts/update.sh \
  -o /tmp/home-dashboard-update
sudo install -m 0755 /tmp/home-dashboard-update /usr/local/sbin/home-dashboard-update
```

Voraussetzungen sind `bash`, `curl`, `ca-certificates`, `tar`, `sha256sum`
und der bereits eingerichtete systemd-Dienst.

## Anwendung aktualisieren

```sh
# Neuestes Release:
sudo home-dashboard-update

# Bestimmtes Release:
sudo home-dashboard-update v0.1.0
```

Das Skript prüft Linux/ARM64 und die Version, lädt Archiv und SHA-256-Prüfsumme,
prüft das Archiv und extrahiert ausschließlich das Binary. Die bisherige Version
wird temporär gesichert und das neue Binary atomar installiert. Anschließend
wird der Dienst neu gestartet und nach zwei Sekunden auf Aktivität geprüft.
Bei einem erkannten Startfehler wird das bisherige Binary wiederhergestellt
und der Dienst erneut gestartet. Ein fehlgeschlagenes Update liefert einen
Fehlerstatus, auch wenn das Rollback erfolgreich war.

Die Prüfung erkennt unmittelbare Startfehler, garantiert aber keinen dauerhaft
fehlerfreien Betrieb oder funktionierende HDMI-Ausgabe. Konfiguration,
systemd-Dienstdatei, udev-Regeln und das Update-Skript selbst bleiben unverändert.
Updates werden ausschließlich manuell installiert.

**Ein Update unterbricht die Anzeige und laufende Audiowiedergabe.** Nach dem
Neustart ist die Szene leer; der Client muss sie erneut senden. Auch ein Rollback
stellt die vorherige Szene nicht wieder her.

## Status überprüfen

```sh
sudo systemctl status pi-dashboard
sudo journalctl -u pi-dashboard -n 50 --no-pager
curl -fsS http://localhost:8080/health
file /usr/local/bin/pi-dashboard
```
