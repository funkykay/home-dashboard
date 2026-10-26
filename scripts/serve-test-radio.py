#!/usr/bin/env python3
"""Local synthetic MP3/AAC streams for manual tests; never part of production playback."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import time

FIXTURES = Path(__file__).resolve().parent.parent / 'tests' / 'fixtures'

class Radio(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def do_GET(self):
        name = self.path.lstrip('/')
        if name not in ('tone.mp3', 'tone.aac'):
            self.send_error(404)
            return
        data = (FIXTURES / name).read_bytes()
        self.send_response(200)
        self.send_header('Content-Type', 'audio/mpeg' if name.endswith('mp3') else 'audio/aac')
        self.send_header('Connection', 'close')
        self.end_headers()
        try:
            while True:
                for start in range(0, len(data), 1024):
                    chunk = data[start:start + 1024]
                    self.wfile.write(chunk)
                    self.wfile.flush()
                    time.sleep(len(chunk) / len(data))
        except (BrokenPipeError, ConnectionResetError):
            pass

if __name__ == '__main__':
    print('Synthetic radio: http://127.0.0.1:8090/tone.mp3 and /tone.aac')
    ThreadingHTTPServer(('127.0.0.1', 8090), Radio).serve_forever()
