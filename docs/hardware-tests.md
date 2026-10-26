# Hardware-Abnahme auf Raspberry Pi 3B

Diese Schritte sind eine auszuführende Testanleitung, kein Protokoll bereits bestandener Hardwaretests. Pro Durchlauf OS-/Kernelversion, TV-Modell, Kabel, Audio-Gerät, Binärdatei-SHA256 und Ergebnisse festhalten. Container-Tests ersetzen diese Abnahme nicht.

## Vorbereitung

```sh
uname -a
cat /etc/os-release
sha256sum /usr/local/bin/pi-dashboard
sudo apt-get install -y fbset libdrm-tests sysstat curl jq
aplay -l
ls -l /dev/fb* /dev/dri/card* /dev/snd/ /dev/tty1
id dashboard
```

TV einschalten, Netzwerk verbinden, README-Installation abschließen. Für Audio ausschließlich selbst konfigurierte, erreichbare direkte MP3-/AAC-LC-Streams nutzen. Alternativ die synthetischen Testdateien mit dem mitgelieferten Testserver auf dem Pi bereitstellen; Python ist nur für diesen Testserver erforderlich.

## Display und Text

1. `cat /proc/cmdline` prüfen: 1920x1080, nicht 4K. `fbset -i` muss die tatsächliche Geometrie zeigen. Bei DRM `sudo modetest -M vc4 -c` verwenden, während der Dashboard-Dienst gestoppt ist; vorhandene Connector-/Mode-Liste protokollieren.
2. Dienst starten. Ohne Szene exakt **Wie sie sehen, sehen Sie nichts.** zentriert prüfen. Mit `/v1/status` das tatsächlich gewählte Backend notieren.
3. Beispielszene laden. Temperatur, Wettertext, drei Vorhersagetage und Quellenangabe prüfen. Umlaute, Gradzeichen und Farbe müssen lesbar sein. Bei Bedarf `config.name` des Radios auf `ÄÖÜ äöü ß — Hörtest` setzen.
4. Radio per PATCH über die Wetterkomponente verschieben und `background` auf `#FF000080` setzen. Die untere Komponente muss durchscheinen. Radio erneut verschieben: keine Geisterbilder, untere Inhalte werden wiederhergestellt. Überlappung und Clipping an Bildschirmgrenzen prüfen.
5. Mit `DASHBOARD_DISPLAY=fb` und anschließend `drm` separat wiederholen, soweit verfügbar. Nicht vorhandene Schnittstelle ausdrücklich als nicht verfügbar markieren. DRM soll 1080p wählen und den vorherigen CRTC beim Stoppen wiederherstellen. Nicht beide Backends gleichzeitig starten.
6. TV aus-/einschalten, Dienst neu starten; Ausgabe muss wiederkehren. Automatische Hotplug-Reinitialisierung wird nicht zugesagt.

## HDMI-Audio und Ownership

1. Dienst stoppen. `speaker-test -D '<HDMI-ALSA-Gerät>' -c 2 -r 48000 -t sine` auf beiden Kanälen hören. Gerätenamen, TV-Lautstärke und Ergebnis protokollieren.
2. Dienst starten und zwei Radios mit unterschiedlichen IDs anlegen: A mit MP3, B mit AAC-LC. Beide beginnen `stopped`. A starten, Ton und `playing` prüfen; dann B starten. A muss `stopped` werden, nur B darf hörbar sein.
3. A wieder starten. B muss vollständig stoppen. Mit `sudo fuser -v /dev/snd/*` gegebenenfalls feststellen, welcher Prozess das Gerät hält. Im Serverstatus darf genau ein Owner erscheinen.
4. `pause`, `play`, `volume` mit 0, 0.25 und 1, dann `stop` prüfen. Pause/Stop geben den Output frei. Nach `play` wird wieder der aktuelle Livestream gehört.
5. Während A spielt Position, Farbe, Name und Lautstärke ändern. Keine neue Verbindung/Decoderinitialisierung durch reine Darstellungsänderungen. Stream-URL ändern: A stoppt; nach erneutem `play` neue URL hören.
6. Konkurrierende Claims aus zwei Terminals oder mit `xargs -P 8` ausführen. Mehrfach zwischen A/B wechseln. Kein überlagerter Ton, keine hängende API und kein dauerhaft offenes Gerät nach Szenenende.
7. Bei laufendem Radio neue Szene laden, dann Szene löschen. Ton muss aufhören, Netzwerkverbindung geschlossen werden. Auch eine leere neue Szene muss vollständig aufräumen.
8. Ein ungültiges Szenen-PUT senden, z. B. doppelte ID oder Breite 9999. HTTP 422 prüfen; laufende Szene und Ton bleiben erhalten.

## Ausfälle und Shutdown

1. Netzwerk kurz unterbrechen. Wetterdaten bleiben mit Fehlerkennzeichnung sichtbar. Radio zeigt nach Read-Timeout Fehler und versucht mit begrenztem Backoff erneut zu verbinden. Nach Wiederherstellung muss es wieder spielen.
2. Während des Backoffs anderes Radio beanspruchen oder Szene löschen. Der vorherige Owner darf anschließend keine erneuten Verbindungen aufbauen.
3. Ungültige Streamdaten, HTTP 404 und eine AAC+-Quelle testen: Fehlerstatus und Retry statt Prozessabsturz. Danach eine gültige Szene/Quelle aktivieren.
4. SSE öffnen: `curl -N http://localhost:8080/v1/events`. Szene aktivieren, Komponente ändern, Owner wechseln; sinnvolle Events beobachten. Verbindung neu öffnen: `resync`, danach Zustand per GET laden. Langsamen Client simulieren und auf Resync nach Pufferüberlauf prüfen.
5. Während Audio läuft und SSE verbunden ist: `sudo systemctl stop pi-dashboard`. Prozess und Audio müssen innerhalb `TimeoutStopSec=15` beendet sein; kein erzwungenes SIGKILL im Journal. Neustart zeigt die leere Szene.
6. Pi neu booten: systemd startet den Server automatisch, es gibt keine wiederhergestellte Szene.

## Ressourcen und 24-Stunden-Test

```sh
pid=$(systemctl show -p MainPID --value pi-dashboard)
pidstat -p "$pid" -u -r -d 5
ps -o pid,nlwp,rss,vsz,pcpu,etime -p "$pid"
sudo ls "/proc/$pid/fd" | wc -l
curl -s http://localhost:8080/v1/status | jq
vcgencmd measure_temp
vcgencmd get_throttled
journalctl -u pi-dashboard --since '1 hour ago'
```

Jeweils mindestens fünf Minuten erfassen: leerer Bildschirm; Wetter nach erfolgreichem Abruf; MP3-Wiedergabe; AAC-LC-Wiedergabe; ständiger Szenenwechsel. Ohne Änderungen muss `rendered_frames` konstant bleiben. Für die begrenzte Ladeanimation dürfen kleine Regionen aktualisiert werden; bei `playing` gibt es keine periodische Radioanimation. Keine feste Hardware-CPU-Grenze wird ohne Messung behauptet.

Mindestens 24 Stunden mit Wetter und wechselnden Streams betreiben. Dazu regelmäßig Szene austauschen und löschen, 1000 Komponentenupdates und 1000 Ownerwechsel ausführen. RSS, Threads und offene Deskriptoren müssen sich nach Aufwärmen stabilisieren; kein stetiges Wachstum. Als Betriebsbudget dient das systemd-Limit von 256 MiB; ein OOM-Kill ist ein Fehler. CPU-Auslastung, Audiounterbrechungen, thermisches Throttling und Zeit bis zur API-Antwort dokumentieren. Bei Bedarf `perf stat` für CPU-Profilierung verwenden, bevor weitere SIMD-/Low-Level-Optimierungen vorgenommen werden.

## Ergebnisprotokoll

| Prüfung | Ergebnis / Messwert | Hardware / Version / Bemerkung |
|---|---|---|
| Full-HD/Framebuffer | offen | |
| Full-HD/DRM-KMS | offen | |
| Umlaute/Alpha/Dirty Regions | offen | |
| HDMI MP3 | offen | |
| HDMI AAC-LC | offen | |
| Exklusives Ownership | offen | |
| Ausfälle/Retry/Shutdown | offen | |
| CPU/RSS/Deskriptoren | offen | |
| 24 Stunden stabil | offen | |
| systemd-Boot | offen | |
