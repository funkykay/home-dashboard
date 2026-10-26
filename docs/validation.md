# Ausgeführte Prüfungen

Stand: 26. Oktober 2026. Die Prüfung erfolgte auf dem Entwicklungs-Mac und in einer Linux-AArch64-VM mit Docker. Kein Raspberry Pi und kein physischer HDMI-Bildschirm beziehungsweise HDMI-Audioausgang waren angeschlossen.

## Automatisierte Qualitätssicherung

| Prüfung | macOS AArch64 / Rust 1.98.1 | Linux AArch64 / Rust 1.90.0, Debian Bookworm |
|---|---|---|
| `cargo fmt --all -- --check` | bestanden | bestanden |
| `cargo clippy --all-targets --all-features -- -D warnings` | bestanden | bestanden |
| `cargo test --all` | 19 Tests bestanden | 21 Tests bestanden |
| `cargo build --release` | bestanden | bestanden |

Der Linux-Build führt die Cargo-Operationen zusätzlich mit `--locked` aus. Die zwei zusätzlichen Linux-Tests prüfen die Framebuffer-UAPI-Strukturlayouts/RGB565-Konvertierung und das tatsächliche ALSA-Backend mit dem Softwaregerät `null`. Das ist keine Bestätigung hörbarer HDMI-Ausgabe.

Die Tests umfassen Szenenvalidierung und atomare Ersetzung, ungültige Szenen, partielle Updates ohne Neustart anderer Komponenten, Befehle, konkurrierende Audio-Claims, begrenzte Nachrichtenqueues, verspätete interne Nachrichten, HTTP-Fehler und Requestlimits, SSE einschließlich Überlauf/Verbindungslimits, Wetter-Cache/Retry, ICY-Metadaten, Ringbuffer sowie Dirty-Region-Komposition und ruhenden Renderer.

Der Audio-Integrationstest wiederholt **20 vollständige Durchläufe** mit echten MP3-/AAC-LC-Decodern und lokalen synthetischen HTTP-Streams. Jeder Durchlauf prüft Geräte-Ownership, zustandserhaltendes Update, simulierten Gerätefehler, automatischen Retry, Szenenersetzung und Freigabe von Audio- und Netzwerkressourcen. Dabei darf höchstens ein Test-Ausgabegerät gleichzeitig offen sein.

Ein dabei gefundener Hänger beim Abbruch eines Decoder-Reads wurde behoben: `Interrupted` fordert bei Rust-Leseoperationen einen neuen Versuch an; endgültiger Abbruch verwendet nun `ConnectionAborted`. Der Ringbuffer-Test prüft sowohl `read` als auch `read_exact`. Außerdem schützt ein Cancellation-Drop-Guard die Worker beim Abbruch ihres Supervisors, und ursprüngliche Gerätefehler werden gegenüber nachfolgenden Cleanup-Abbrüchen bevorzugt gemeldet.

## Zusätzliche Laufzeitprüfungen

Am 25. Oktober wurde die macOS-Release-Binärdatei im Headless-Modus gestartet:

- Health-API, Szenenaktivierung und SSE-Verbindungsaufbau erfolgreich.
- Tatsächlicher Open-Meteo-Abruf für Berlin lieferte gültige aktuelle Daten und drei Vorhersagetage; Status `ready`.
- PPM-Ausgabe des Leerbildschirms und der Beispielszene visuell geprüft: exakter Leertext, Umlaute, Temperatur, Vorhersage, Quellenangabe und Radioanzeige sichtbar.
- Ohne Änderungen blieb der Renderzähler bei exakt einem Frame.
- SIGTERM bei offener SSE-Verbindung und unvollständig übertragenem HTTP-Body beendete den Prozess nach etwa 5,02 Sekunden mit Exitcode 0. Die HTTP-Abwicklung hat hierfür ein Zeitlimit von fünf Sekunden.
- Eine einzelne RSS-Stichprobe mit geladener Wetter-Szene und aktiver PPM-Diagnoseausgabe ergab etwa 35,5 MiB auf macOS. Dieser Wert ist weder ein Pi-Benchmark noch eine zugesagte Speicherobergrenze.

## Release und verbleibende Abnahme

Das aktualisierte Linux-AArch64-Release wurde am 26. Oktober nach allen Korrekturen erfolgreich exportiert. `dist/pi-dashboard` ist ein ELF-64-AArch64-Programm. Die beigefügte SHA256-Datei wurde mit dem tatsächlichen Artefakt verglichen. Installer, systemd-Datei, Konfiguration, Beispielszene und Dokumentation liegen daneben; Shellskripte wurden mit `sh -n` geprüft.

HDMI-Bild, fb0-/DRM-Treiberverhalten, hörbares HDMI-Audio, Pi-CPU-/RAM-/Temperaturwerte, systemd-Boot auf Raspberry Pi OS und 24-Stunden-Dauerbetrieb bleiben **offen**. Dafür gilt die [Hardware-Testanleitung](hardware-tests.md). Frontbuffer-Tearing und manuell erforderlicher Neustart nach HDMI-Hotplug sind in der README dokumentierte Grenzen.
