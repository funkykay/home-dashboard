# Home Dashboard

Eine Rust-Engine für einen Raspberry Pi 3B mit Raspberry Pi OS Lite **64-Bit**. Ein HTTP-Client übermittelt eine deklarative Szene; der Pi zeichnet sie direkt über Framebuffer oder DRM/KMS auf HDMI in **1920 × 1080**. Wetterdaten kommen von Open-Meteo, Webradio wird mit Symphonia decodiert und exklusiv über ALSA ausgegeben. Kein Desktop, Browser, Audio-Daemon oder externer Player erforderlich.

Das Programm startet immer ohne Szene und zeigt zentriert:

> Wie sie sehen, sehen Sie nichts.

## Entwicklung auf macOS

Voraussetzungen: Rust über rustup, Xcode Command Line Tools; für das Linux-Release Docker Desktop oder Colima mit Buildx. Das eingecheckte `rust-toolchain.toml` fixiert die lokale Toolchain. Die Linux-Build-Umgebung verwendet Rust 1.90.0, die unterstützte Mindestversion. `Cargo.lock` fixiert die Crates.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all
cargo build --locked --release
cargo run --locked --release -- --headless
```

`--headless` nutzt dieselbe API, Szenenlogik, Wetterverarbeitung, Decoder und Render-Engine; nur Geräteausgabe wird durch einen Speicherbildschirm und einen zeitlich getakteten Audio-Sink ersetzt. Dabei ist kein Ton hörbar. Die Tests benötigen Loopback-Netzwerkzugriff sowie Arial auf macOS beziehungsweise DejaVu Sans auf Linux. Sie benötigen keinen Internetzugriff.

Optionale Bildausgabe für die Entwicklung, als PPM ohne zusätzliche Bildbibliothek:

```sh
DASHBOARD_SNAPSHOT=/tmp/dashboard.ppm cargo run --release -- --headless
```

Die Datei wird bei Änderungen überschrieben. Sie ist zur Diagnose gedacht und sollte bei Performance-Messungen deaktiviert sein. Eine vorhandene TTF-/OTF-Datei lässt sich mit `DASHBOARD_FONT=/pfad/font.ttf` wählen. Unicode-Glyphen hängen von deren Zeichenumfang ab; Deutsch mit Umlauten, Kerning sowie links-/mittig-/rechtsbündiger Text werden unterstützt. Komplexes Schrift-Shaping, bidirektionales Layout und automatischer Zeilenumbruch gehören nicht zum Funktionsumfang.

Für einen vollständig lokalen Audiotest in einem zweiten Terminal:

```sh
python3 scripts/serve-test-radio.py
curl -fsS -X PUT http://localhost:8080/v1/scene \
  -H 'Content-Type: application/json' --data-binary @examples/scene.json
curl -fsS -X POST http://localhost:8080/v1/components/living-room-radio/commands \
  -H 'Content-Type: application/json' -d '{"command":"play"}'
```

Die Beispielszene enthält Wetter für Berlin und eine vom Client konfigurierte Loopback-Stream-URL für dieses Testsignal. Für den echten Betrieb die URL durch den gewünschten direkten Radio-Stream ersetzen. Im Server sind keine Sender hinterlegt.

## Linux-AArch64-Release auf dem Mac bauen

```sh
./scripts/build-aarch64.sh
file dist/pi-dashboard
```

Der Build läuft vollständig in einem **Linux-ARM64-Container**, auf Apple Silicon nativ in der Linux-VM, auf Intel-Macs mit Docker-Emulation. Er installiert ARM64-ALSA-Entwicklungsbibliotheken und führt Formatierung, Clippy, Tests und Release-Build aus. DRM benutzt direkte Kernel-ioctls und benötigt kein zusätzliches libdrm zum Linken. Das Basis-Image ist per SHA256 fixiert; Cargo verwendet `--locked`. APT-Sicherheitsupdates bleiben bewusst verfügbar; dies ist kein Versprechen byteidentischer Artefakte über beliebige spätere Paketstände hinweg.

`dist/` enthält die ausführbare Datei, SHA256-Prüfsumme, Dienstdatei, Konfiguration, Installer und Beispielszene. Die Binärdatei verwendet glibc aus Debian Bookworm (2.36) und libasound; Raspberry Pi OS Bookworm oder neuer mit AArch64 ist erforderlich. Auf dem Pi wird nichts kompiliert. Das Verzeichnis ist von Git ausgeschlossen.

## Raspberry Pi OS Lite einrichten

1. Raspberry Pi OS Lite **64-Bit** installieren, Netzwerk und optional SSH einrichten. HDMI-TV anschließen. `uname -m` muss `aarch64` melden.
2. In `/boot/firmware/config.txt` den vollständigen KMS-Treiber aktivieren beziehungsweise den vorhandenen Eintrag beibehalten:

   ```ini
   dtoverlay=vc4-kms-v3d
   disable_fw_kms_setup=1
   ```

3. An die **bestehende einzelne Zeile** in `/boot/firmware/cmdline.txt` anhängen:

   ```text
   video=HDMI-A-1:1920x1080M@60 consoleblank=0 vt.global_cursor_default=0
   ```

   `root=...` und andere Bootparameter erhalten. Für den dedizierten Dashboard-Bildschirm `console=tty1` entfernen, damit Kernelmeldungen nicht auf die Grafik schreiben; eine serielle Konsole kann bestehen bleiben. Bei älteren Installationen liegen die Dateien unter `/boot/`. Den Connectornamen mit `/sys/class/drm/` prüfen. Der TV skaliert die Full-HD-Ausgabe auf sein Panel. Die [offizielle KMS-Dokumentation](https://www.raspberrypi.com/documentation/computers/configuration.html) beschreibt die `video=`-Syntax; alte Firmware-/FKMS-Moduseinstellungen werden nicht benötigt.
4. Release übertragen und installieren:

   ```sh
   # Auf dem Mac:
   scp -r dist pi@raspberrypi.local:~/pi-dashboard-release
   # Auf dem Pi:
   cd ~/pi-dashboard-release
   sudo ./install.sh
   ```

   Alternativ vom Mac: `./scripts/deploy.sh pi@raspberrypi.local`.

Der Installer prüft Architektur und SHA256, installiert ALSA, DejaVu Sans und CA-Zertifikate, erstellt den Benutzer `dashboard`, richtet Geräteberechtigungen ein und aktiviert den systemd-Dienst. Eine bestehende `/etc/pi-dashboard.env` bleibt erhalten. Er deaktiviert den Login-Getty auf **tty1**; andere Konsolen bleiben verfügbar. Bootkonfiguration und ALSA-Gerätewahl werden nicht automatisch überschrieben.

### HDMI-Audio wählen

```sh
aplay -l
aplay -L
# Den tatsächlich angezeigten HDMI-Kartennamen verwenden:
speaker-test -D 'plughw:CARD=vc4hdmi,DEV=0' -c 2 -r 48000 -t sine
sudo nano /etc/pi-dashboard.env
```

In der Konfiguration `DASHBOARD_ALSA=plughw:CARD=vc4hdmi,DEV=0` anpassen. Je nach Kernel heißt die Karte anders, beispielsweise `vc4hdmi0`. `plughw` erlaubt ALSA die erforderliche Kanal-/Formatanpassung; der Player fordert die native Stream-Samplerate an. Es gibt keine Abhängigkeit von PulseAudio, PipeWire oder einem Mixer. Während `speaker-test` läuft, den Dashboard-Dienst stoppen.

### Dienst betreiben

```sh
sudo reboot
sudo systemctl status pi-dashboard
sudo systemctl stop pi-dashboard
sudo systemctl start pi-dashboard
sudo systemctl restart pi-dashboard
sudo systemctl enable pi-dashboard
journalctl -u pi-dashboard -f
curl -fsS http://raspberrypi.local:8080/health
```

Der Dienst läuft als eigener Benutzer mit Gruppenrechten für Audio und Display. Die udev-Regeln erlauben Zugriff auf `/dev/fb*`, `/dev/dri/card*` und `/dev/tty1`. systemd gibt dem Prozess tty1 als kontrollierende Konsole; die Anwendung schaltet sie in Grafikmodus und stellt beim regulären Ende den alten Modus wieder her. Dienstlimits: 256 MiB RAM, 32 Tasks, 256 Dateideskriptoren. Der Dienst startet nach Fehlern neu; dabei bleibt die Szene absichtlich leer.

## Konfiguration

Alle Einstellungen erfolgen über Umgebungsvariablen beziehungsweise `/etc/pi-dashboard.env`.

| Variable | Standard / Bedeutung |
|---|---|
| `DASHBOARD_BIND` | `0.0.0.0:8080` |
| `DASHBOARD_DISPLAY` | `auto`; alternativ `fb`, `drm`, `headless` |
| `DASHBOARD_ALSA` | `plughw:CARD=vc4hdmi,DEV=0` |
| `DASHBOARD_FONT` | DejaVu Sans unter Linux, Arial auf macOS |
| `DASHBOARD_TTY` | `/dev/tty1` |
| `DASHBOARD_DRM_DEVICE` | Optional feste Karte; sonst Suche `card0` bis `card7` |
| `DASHBOARD_CONNECTOR` | Optional etwa `HDMI-A-1`; sonst verbundener Connector |
| `DASHBOARD_SNAPSHOT` | Optionale PPM-Datei beim Headless-Backend |
| `RUST_LOG` | `pi_dashboard=info` |

`--headless` schaltet Display **und** Audio auf Entwicklungsbackends. `DASHBOARD_DISPLAY=headless` schaltet ausschließlich die Anzeige um und ermöglicht auf Linux einen separaten ALSA-Test.

`auto` prüft Auflösung, Stride, virtuelle Offsets, Speichermenge, Truecolor-Bitfelder und Schreibzugriff von fb0. Akzeptiert werden progressive 1080p mit 16/32 bpp. Bei ungeeignetem oder fehlendem fb0 wird DRM/KMS verwendet: passende CRTC-/Encoder-Zuordnung, Full-HD-Modus, XRGB8888-Dumb-Buffer und Wiederherstellung des vorherigen CRTC-Zustands beim Shutdown. Die Ausgabe schreibt nur geänderte Regionen. Es wird ein Frontbuffer verwendet; bei Animationen ist Tearing möglich. Ob ein konkreter Treiber mmap-Änderungen sichtbar ausgibt, muss am Gerät geprüft werden; nötigenfalls `DASHBOARD_DISPLAY=drm` erzwingen. HDMI-Hotplug wird nicht automatisch neu initialisiert: TV einschalten, danach Dienst neu starten.

## Szene und Komponenten

Die vollständige [Beispielszene](examples/scene.json) enthält Wetter und Radio. Eine Szene hat `schema_version: 1` und maximal 32 Komponenten. Jede Komponente hat eine eindeutige ID aus 1–64 ASCII-Buchstaben, Ziffern, `_` oder `-`, `type`, `config`, `x`, `y`, `width`, `height` und optional `style`. Geometrie ist ganzzahlig, nicht leer und vollständig innerhalb von 1920 × 1080. Die Reihenfolge im Array ist die Zeichenreihenfolge: spätere Komponenten liegen darüber. Inhalte werden auf ihre Komponentengrenzen beschnitten.

| Gemeinsamer Style | Standard | Werte |
|---|---|---|
| `background` | `#152338EE` | `#RRGGBB` oder `#RRGGBBAA` |
| `text_color` | `#F1F5F9` | ebenso |
| `accent` | `#38BDF8` | ebenso |
| `font_size` | `30` | 8–128 Pixel |
| `padding` | `24` | 0–128 Pixel |
| `align` | `left` | `left`, `center`, `right` |

Unbekannte Felder und ungültige Werte werden zurückgewiesen. Nicht angegebene Style-Felder verwenden die Standards. Ein `PATCH` ergänzt/überschreibt nur mitgesendete Konfigurations- und Style-Felder; `null` bei optionalen Patch-Feldern bedeutet keine Änderung. Typ und ID sind unveränderlich. Position, Größe, Style, Radioname und Lautstärke erhalten den Laufzeitzustand. Eine neue Stream-URL stoppt nur dieses Radio; ein erneutes `play` startet die neue URL. Standort oder Wetterintervall starten nur den betreffenden Wetter-Worker neu. Andere Komponenten bleiben aktiv.

### Wetter (`weather`)

`latitude` und `longitude` sind Pflichtfelder. `refresh_seconds` ist optional, Standard 900, Bereich 300–86400. Der Server fragt die feste Open-Meteo-Forecast-API ab und zeigt aktuelle Temperatur, WMO-Wetterzustand und drei Vorhersagetage in lokaler Standortzeit.

Standorte werden auf fünf Nachkommastellen als Cache-Schlüssel normalisiert. Ein geteilter Cache hält höchstens 64 Standorte; gleichzeitige Anfragen werden zusammengeführt. Es gibt maximal einen Abruf gleichzeitig und mindestens zwei Sekunden Abstand. Eine Antwort ist auf 128 KiB begrenzt. Bei Fehlern bleiben vorhandene Daten sichtbar und der Status kennzeichnet sie als veraltet. Automatische Wiederholungen verwenden exponentiellen Backoff mit Jitter bis maximal 300 Sekunden.

Die freie API ist für private, nichtkommerzielle Nutzung vorgesehen. Die Darstellung enthält **Open-Meteo.com · CC BY 4.0**. Daten stammen von [Open-Meteo](https://open-meteo.com/en/docs); [Nutzungsbedingungen](https://open-meteo.com/en/terms) und [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) beachten. Änderungen der Lizenz-/API-Bedingungen bleiben Sache des Anbieters.

### Webradio (`webradio`)

Pflichtfelder: `name` (1–128 Zeichen) und `stream_url` (HTTP/HTTPS, maximal 2048 Zeichen, ohne eingebettete Benutzeranmeldung). `volume`: 0–1, Standard 0,7. Unterstützt sind direkte MP3- und AAC-LC/ADTS-Streams, HTTP-Redirects und ICY-Metadaten. Keine HLS-/M3U-/PLS-Auflösung, kein HE-AAC/AAC+ und keine Authentifizierung für Streams. Bei Playlists die eigentliche Audio-URL verwenden. Mono/Stereo mit 8–96 kHz werden akzeptiert.

| Befehl | JSON | Verhalten |
|---|---|---|
| Start/Fortsetzen | `{"command":"play"}` | Output beanspruchen, Stream verbinden; bei bestehendem Ownership idempotent |
| Pause | `{"command":"pause"}` | Pipeline schließen, Output freigeben, Status `paused` |
| Stopp | `{"command":"stop"}` | Pipeline schließen, Output freigeben, Status `stopped` |
| Lautstärke | `{"command":"volume","value":0.4}` | Laufend ändern und in Komponentenkonfiguration übernehmen |

Ein Live-Stream wird bei `play` nach Pause neu verbunden; es gibt keinen zeitversetzten Audiopuffer. Zustände: `stopped`, `buffering`, `playing`, `paused`, `error`. Bei Übernahme durch ein anderes Radio wird der vorherige Owner intern benachrichtigt und vollständig gestoppt. Netzwerk, Decoder und Audioausgabe werden beendet und gejoint, bevor der nächste Owner starten kann. Fehler beim Start führen zu `error` mit automatischem Retry; dieser bleibt ausschließlich aktiv, solange das Radio Owner ist.

## HTTP-API

Basis-URL in den Beispielen: `http://raspberrypi.local:8080`. Keine Authentifizierung und kein CORS-Webfrontend; nur im vertrauenswürdigen Heimnetz betreiben. Clientverbindungen sind für die Lebensdauer der Szene unerheblich.

| Methode / Pfad | Funktion |
|---|---|
| `GET /health` | 200 bei laufender Engine; 503 bei erkanntem Displayfehler |
| `GET /v1/status` | Version, Szene, Audio-Owner, Display, Renderzähler, Event-Sequenz |
| `PUT /v1/scene` | Vollständig validieren und atomar aktivieren/ersetzen |
| `GET /v1/scene` | Aktuelle Szene oder JSON `null` |
| `DELETE /v1/scene` | Alle Komponenten beenden; Leerbildschirm |
| `GET /v1/components` | Komponentendefinitionen und Status |
| `GET /v1/components/{id}` | Einzelne Definition |
| `PATCH /v1/components/{id}` | Teilupdate von Geometrie, Style, `config` |
| `GET /v1/components/{id}/status` | Zustand, Daten beziehungsweise Wiedergabestatus |
| `POST /v1/components/{id}/commands` | Komponentenbefehl |
| `POST /v1/commands` | Befehl einschließlich `id` im JSON |
| `GET /v1/audio` | Exklusiver `owner` oder `null`, Session-Generation |
| `GET /v1/events` | SSE-Stream |

```sh
curl -fsS -X PUT http://raspberrypi.local:8080/v1/scene \
  -H 'Content-Type: application/json' --data-binary @examples/scene.json

curl -fsS -X PATCH http://raspberrypi.local:8080/v1/components/living-room-radio \
  -H 'Content-Type: application/json' \
  -d '{"y":160,"style":{"text_color":"#A7F3D0"},"config":{"volume":0.3}}'

curl -fsS -X POST http://raspberrypi.local:8080/v1/commands \
  -H 'Content-Type: application/json' \
  -d '{"id":"living-room-radio","command":"play"}'

curl -fsS http://raspberrypi.local:8080/v1/components/weather-berlin/status
curl -fsS http://raspberrypi.local:8080/v1/audio
curl -fsS -X DELETE http://raspberrypi.local:8080/v1/scene
```

Erfolgreiche Operationen liefern 200 mit JSON. Fehler: 400 syntaktisch ungültiges JSON, 404 fehlende Komponente/Route, 405 falsche Methode, 408 Request-Timeout, 413 zu großer Body, 415 falscher Content-Type, 422 ungültige Konfiguration/Befehle, 503 ausgelastete oder beendete Engine. Format:

```json
{"error":{"code":"invalid_configuration","message":"duplicate component id: radio"}}
```

Begrenzungen: 64 KiB Request-Body, 64 offene TCP-Verbindungen, 32 gleichzeitig bearbeitete HTTP-Anfragen, 64 wartende Engine-Anfragen, 16 SSE-Clients. Änderungen werden im Engine-Actor serialisiert. Ungültige neue Szenen lassen die aktive Szene unangetastet. Nach einem HTTP-Timeout zuerst den aktuellen Status abfragen: ein bereits angenommener Befehl kann trotzdem abgeschlossen worden sein.

### Server-Sent Events

```sh
curl -N http://raspberrypi.local:8080/v1/events
```

Events: `scene.activated`, `scene.stopped`, `component.updated`, `component.state_changed`, `audio.owner_changed`, `webradio.playback_error`. `data` ist JSON, `id` eine innerhalb dieses Serverlaufs monoton steigende Nummer. Der Server sendet keine periodischen Zustandsänderungen; alle 15 Sekunden kann ein SSE-Keepalive-Kommentar erscheinen.

Bei Verbindungsaufbau kommt `resync`, ebenso wenn ein langsamer Client den 128-Event-Puffer überläuft. Dann `GET /v1/scene`, `/v1/components` und `/v1/audio` neu laden. Reconnect wird mit `retry: 2000` unterstützt. Es gibt **keine Event-Historie über Reconnects**; `Last-Event-ID` wird nicht wiedergegeben. Die Status-API ist die maßgebliche Quelle. Mehrere GETs sind jeweils konsistent, aber keine gemeinsame Transaktion; bei parallelen Mutationen gegebenenfalls erneut abgleichen.

## Architektur und Ressourcen

Ein ausführbares Crate mit internen Modulen. `Component` kapselt Status, Ereignisse, Befehle, Updates, Rendering und Worker-Cleanup. Die statische Registrierung liegt in `component::create`/`validate` und der getypten `Config`-Enum. Neue Typen ergänzen diese Stellen und implementieren das Trait.

Der Engine-Actor verwaltet eine Szene und zentral das Audio-Ownership. Interne Nachrichten sind typisierte Rust-Enums in einem 128-Einträge-Channel. Generations- und Audio-Session-IDs verwerfen verspätete Worker-Ergebnisse. Wetter-Worker besitzen CancellationTokens und werden beim Entfernen gejoint. Der Renderer erhält nur den neuesten Snapshot; Aktualisierungen können sich zusammenfassen, ohne den maßgeblichen Engine-Zustand zu verlieren.

Ein Software-Backbuffer benötigt etwa 7,9 MiB. Dirty Regions werden transitiv vereinigt; jede Region wird vom Hintergrund aus in Zeichenreihenfolge neu zusammengesetzt. Glyphen werden mit fontdue gerastert; der Cache ist auf 4 MiB Bitmapdaten und 2048 Einträge begrenzt. Unveränderte Szenen erzeugen keine Frames. Die kleine Ladeanimation wird zeitbasiert auf ihrem eigenen Bereich mit maximal etwa 29,4 FPS gezeichnet; verspätete Frames werden übersprungen. Rechtecke, Linien, Kreise, Alpha-Komposition und Text sind implementiert. Keine Bild-/Icon-Abhängigkeiten.

Audio verwendet einen 256-KiB-Ring für komprimierte Daten, acht wiederverwendbare PCM-Blöcke von maximal je 32 KiB, einen 64-KiB-Decoder-Lesepuffer und einen ALSA-Puffer mit ungefähr 200 ms Zielgröße. Zwei blockierende Worker übernehmen Decoding und Ausgabe; Netzwerk-I/O läuft asynchron. Der Output-Worker wartet nur auf PCM und ALSA, niemals auf HTTP. Die Engine mixt keine Streams. Zwei Tokio-Worker und ein Renderthread bilden die Grundlast; Audio fügt zwei Worker hinzu. Keine durchgängige Zero-Copy-Behauptung: Decoder, Kanalinterleaving und ALSA benötigen Kopien.

Symphonia aktiviert `opt-simd-neon`; ein Geschwindigkeitsvorteil auf dem Cortex-A53 ist damit **nicht gemessen**. Für MP3/AAC-LC wird CPU-Decoding verwendet. Der aktuelle [VC4-HDMI-Treiber](https://github.com/torvalds/linux/blob/master/drivers/gpu/drm/vc4/vc4_hdmi.c) stellt ALSA/PCM-Ausgabe bereit; daraus ergibt sich keine nutzbare MP3/AAC-Decoder-Offload-Schnittstelle. Legacy-OpenMAX-/Firmware-Decoder sind keine Voraussetzung. Codec- und SIMD-Unterstützung: [Symphonia 0.5.5](https://docs.rs/symphonia/0.5.5/symphonia/). Framebuffer-Schnittstelle: [Linux-Dokumentation](https://docs.kernel.org/fb/api.html).

## Diagnose und Hardware-Abnahme

Die tatsächlich ausgeführten Prüfungen und ihre Grenzen stehen im [Validierungsbericht](docs/validation.md).

Die ausführliche [Hardware-Testanleitung](docs/hardware-tests.md) enthält Display-, Audio-, Ownership-, Ressourcen- und Dauerbetriebstests. Ohne reale Pi-Hardware sind HDMI-Bild, HDMI-Ton, Treiberverhalten und Last-/Temperaturwerte nicht als bestanden zu betrachten.

| Problem | Prüfung / Maßnahme |
|---|---|
| Kein Bild / falsche Auflösung | TV vor Boot einschalten; `cat /proc/cmdline`, `fbset -i`, `modetest -M vc4 -c`; 1080p-Modus korrigieren |
| fb0 akzeptiert, Bild bleibt unverändert | `DASHBOARD_DISPLAY=drm` setzen, Dienst neu starten |
| DRM permission denied / master unavailable | Dienstbenutzer/udev-Gruppen prüfen; konkurrierenden Displayserver stoppen; richtige `cardN` wählen |
| tty1 permission denied | Über den gelieferten systemd-Dienst starten; udev-Regel und kontrollierende Konsole prüfen |
| Schwarzer Bildschirm nach Fehler | `journalctl -u pi-dashboard -b`; `curl /health`; Dienst neu starten; notfalls auf anderer Konsole `sudo chvt 2` |
| Kein Ton | `aplay -l`, HDMI-Gerät in Environment ändern; TV-Lautstärke prüfen; `speaker-test` bei gestopptem Dienst |
| Radio dauerhaft `error` | Statusfehler lesen; direkte MP3/AAC-LC-URL statt Playlist/AAC+; DNS/TLS/Systemzeit prüfen |
| Wetterfehler | Standort und Internetzugang prüfen; Status zeigt Fehler und eventuell alte Daten; Retry erfolgt automatisch |
| HTTP 503 | Zu viele Verbindungen/SSE-Abonnenten oder gestoppte Engine; Clients schließen und Status prüfen |
| Hohe Renderlast im Leerlauf | Ladeanimationen/fortlaufende Clientupdates prüfen; `rendered_frames` vergleichen; Snapshot-Ausgabe deaktivieren |
| GLIBC-Version fehlt | Raspberry Pi OS Bookworm oder neuer in 64-Bit verwenden |

Die Software speichert keine Szenen, Passwörter oder Streamdaten dauerhaft. Ein Serverneustart verlangt eine neue Szene vom Client. Keine Git-Commits werden durch Build- oder Deployskripte erstellt.
