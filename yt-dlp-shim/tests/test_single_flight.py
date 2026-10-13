"""Many people pressing play on the same song at once must cost one yt-dlp run, not one each,
and /health must say how busy the shim is."""
import os
import sys
import threading
import time

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

app_module = pytest.importorskip("app")


def test_concurrent_resolves_run_once(monkeypatch):
    calls = []
    calls_lock = threading.Lock()

    def slow_resolve(args, timeout=20, label="ytdlp", bg=False, capture=False):
        with calls_lock:
            calls.append(args)
        time.sleep(0.3)
        return "https://media.example/audio-1\n"

    monkeypatch.setattr(app_module, "_run", slow_resolve)
    # Followers wait for the leader rather than giving up and resolving on their own.
    monkeypatch.setattr(app_module, "_INFLIGHT_WAIT_SEC", 5.0)
    app_module._url_cache_evict("single-flight-1")

    results = []
    results_lock = threading.Lock()

    def play():
        url = app_module._resolve_url("single-flight-1")
        with results_lock:
            results.append(url)

    threads = [threading.Thread(target=play) for _ in range(8)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(10)

    assert len(calls) == 1
    assert results == ["https://media.example/audio-1"] * 8
    # The leader hands its lock back, so a later resolve of the same id is not stuck.
    assert "single-flight-1" not in app_module._INFLIGHT


def test_different_songs_resolve_side_by_side(monkeypatch):
    calls = []
    calls_lock = threading.Lock()

    def resolve(args, timeout=20, label="ytdlp", bg=False, capture=False):
        with calls_lock:
            calls.append(args[-1])
        time.sleep(0.1)
        return f"https://media.example/{args[-1][-1]}\n"

    monkeypatch.setattr(app_module, "_run", resolve)
    for video_id in ("sf-a", "sf-b"):
        app_module._url_cache_evict(video_id)
    threads = [
        threading.Thread(target=app_module._resolve_url, args=(video_id,))
        for video_id in ("sf-a", "sf-b")
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(10)
    assert len(calls) == 2


def test_health_reports_streams_and_running_ytdlp(monkeypatch):
    seen = {}

    class Done:
        returncode = 0
        stdout = "ok"
        stderr = ""

    def fake_subprocess_run(*args, **kwargs):
        seen["running"] = app_module.app.test_client().get("/health").get_json()["ytdlp_running"]
        return Done()

    monkeypatch.setattr(app_module.subprocess, "run", fake_subprocess_run)
    before = app_module.app.test_client().get("/health").get_json()
    assert before["ok"] is True
    assert before["active_streams"] == 0
    assert before["ytdlp_running"] == 0
    assert before["ytdlp_limit"] >= 1

    assert app_module._run(["--version"], label="test") == "ok"
    assert seen["running"] == 1
    assert app_module.app.test_client().get("/health").get_json()["ytdlp_running"] == 0
