"""Minimal HTTP service with /healthz endpoint."""
import os
import signal
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler

PORT = int(os.environ.get("PORT", "8080"))


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/healthz":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok")
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, format, *args):
        pass  # suppress request logging


def shutdown(signum, frame):
    print("SHUTDOWN", flush=True)
    sys.exit(0)


signal.signal(signal.SIGTERM, shutdown)

print(f"LISTENING port={PORT}", flush=True)
HTTPServer(("", PORT), Handler).serve_forever()
