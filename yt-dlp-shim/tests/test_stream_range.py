"""/stream passes a range past the end back as 416 with the size, instead of a 502."""
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

app = pytest.importorskip("app")


class FakeUpstream:
    def __init__(self, status, headers=None, body=b""):
        self.status_code = status
        self.headers = headers or {}
        self.body = body
        self.closed = False

    def iter_content(self, chunk_size):
        yield self.body

    def close(self):
        self.closed = True


def serve(monkeypatch, upstream, range_header=None):
    monkeypatch.setattr(app, "_resolve_url", lambda video_id, bg=False: "https://googlevideo.example/x")
    monkeypatch.setattr(app, "_open_upstream", lambda url, headers, video_id: upstream)
    headers = {"Range": range_header} if range_header else {}
    return app.app.test_client().get("/stream?id=vid-range", headers=headers)


def test_a_range_past_the_end_is_a_416_with_the_size(monkeypatch):
    upstream = FakeUpstream(416, {"Content-Range": "bytes */6755585"})
    answer = serve(monkeypatch, upstream, "bytes=9999999-")
    assert answer.status_code == 416
    assert answer.headers["Content-Range"] == "bytes */6755585"
    assert answer.data == b""
    assert upstream.closed
    assert app._facts_get("vid-range")["size"] == 6755585


def test_another_upstream_failure_is_still_a_502(monkeypatch):
    answer = serve(monkeypatch, FakeUpstream(500), "bytes=0-")
    assert answer.status_code == 502


def test_a_partial_answer_teaches_the_size(monkeypatch):
    upstream = FakeUpstream(206, {"Content-Range": "bytes 0-1/12345", "Content-Length": "2"}, b"ab")
    answer = serve(monkeypatch, upstream, "bytes=0-1")
    assert answer.status_code == 206
    assert answer.data == b"ab"
    assert app._facts_get("vid-range")["size"] == 12345
