Synthetic, one-second 440 Hz sine waves generated for decoder tests; no third-party recording.

Generation with FFmpeg 5.1 (only a test-fixture creation tool, never a runtime dependency):

```sh
ffmpeg -f lavfi -i sine=frequency=440:duration=1 -ar 44100 -ac 2 -codec:a libmp3lame -b:a 128k tone.mp3
ffmpeg -f lavfi -i sine=frequency=440:duration=1 -ar 48000 -ac 2 -codec:a aac -profile:a aac_low -b:a 96k -f adts tone.aac
```
