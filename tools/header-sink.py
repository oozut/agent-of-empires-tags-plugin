#!/usr/bin/env python3
"""M0 proof fixture: an OpenAI-compatible endpoint that records the headers it receives.

Not shipped behaviour — it exists so "the header arrives" can be a file, a line and a grep
(plan/m0.md M0-T08). Stdlib only, loopback only, one JSON line per request.

    python3 tools/header-sink.py 8787 /tmp/tag-sink.jsonl [--reset]

`--reset` truncates the log first, so a run starts from a known-empty file. The response body is a
minimal chat completion, which is enough for a harness that insists on parsing one.
"""

from __future__ import annotations

import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8787
LOG = sys.argv[2] if len(sys.argv) > 2 else "/tmp/tag-sink.jsonl"

if "--reset" in sys.argv[3:]:
    open(LOG, "w").close()

BODY = json.dumps(
    {
        "id": "sink",
        "object": "chat.completion",
        "model": "sink",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": "ok"},
            }
        ],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    }
).encode()


def wants_stream(raw: bytes) -> bool:
    try:
        body = json.loads(raw or b"{}")
    except ValueError:
        return False
    return bool(body.get("stream"))


class Sink(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):  # noqa: N802 (http.server naming)
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length)
        with open(LOG, "a", encoding="utf-8") as fh:
            fh.write(
                json.dumps(
                    {
                        "path": self.path,
                        "headers": dict(self.headers.items()),
                    }
                )
                + "\n"
            )
        if wants_stream(raw):
            self.stream_reply()
            return
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)

    def stream_reply(self):
        """Server-sent events, so a streaming client finishes instead of retrying."""
        chunk = json.dumps(
            {
                "id": "sink",
                "object": "chat.completion.chunk",
                "model": "sink",
                "choices": [
                    {"index": 0, "delta": {"role": "assistant", "content": "ok"}}
                ],
            }
        )
        done = json.dumps(
            {
                "id": "sink",
                "object": "chat.completion.chunk",
                "model": "sink",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            }
        )
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()
        for event in (chunk, done):
            self.wfile.write(f"data: {event}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.close_connection = True

    def log_message(self, *args):  # keep stdout clean for the shell that started it
        pass


if __name__ == "__main__":
    print(f"header-sink listening on 127.0.0.1:{PORT}, logging to {LOG}", flush=True)
    HTTPServer(("127.0.0.1", PORT), Sink).serve_forever()
