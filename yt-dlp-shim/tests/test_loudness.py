"""ReplayGain for played videos: the ebur128 summary read right, measured on play only, at most
two a minute, never twice at once, and skipped while the shim is busy. No network here."""
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import loudness  # noqa: E402

# The tail of a real ffmpeg run (d-OGBZSDKBU, format 140, 2026-10-09).
SUMMARY = """[Parsed_ebur128_0 @ 0x7dc4a0004600] t: 417.2 TARGET:-23 LUFS M: -9.1 S: -8.4 I: -8.0 LUFS
[Parsed_ebur128_0 @ 0x7dc4a0004600] Summary:

  Integrated loudness:
    I:          -8.0 LUFS
    Threshold: -18.1 LUFS

  Loudness range:
    LRA:         7.0 LU
    Threshold: -28.1 LUFS
    LRA low:   -13.3 LUFS
    LRA high:   -6.4 LUFS

  True peak:
    Peak:        1.1 dBFS
[out#0/null @ 0x62b043d472c0] video:0KiB audio:71900KiB subtitle:0KiB other streams:0KiB
"""


def test_the_summary_gives_integrated_loudness_and_true_peak():
    assert loudness.parse_ebur128(SUMMARY) == (-8.0, 1.1)


def test_no_summary_is_no_measurement():
    assert loudness.parse_ebur128("Error opening input: 403 Forbidden") is None
    assert loudness.parse_ebur128("") is None


def test_silence_has_no_peak():
    silent = SUMMARY.replace("Peak:        1.1 dBFS", "Peak:       -inf dBFS")
    assert loudness.parse_ebur128(silent) == (-8.0, None)


def test_replay_gain_is_relative_to_minus_18_and_the_peak_is_linear():
    assert loudness.replay_gain(-8.0, 1.1) == {"track_gain": -10.0, "track_peak": 1.135011}
    assert loudness.replay_gain(-20.5, -3.0) == {"track_gain": 2.5, "track_peak": 0.707946}
    assert loudness.replay_gain(-14.0, None) == {"track_gain": -4.0, "track_peak": None}


def test_the_ffmpeg_run_measures_peak_and_reads_no_stdin():
    args = loudness.ffmpeg_args("https://example/x", "UA/1")
    assert args[0] == "ffmpeg" and "-nostdin" in args
    assert args[args.index("-af") + 1].startswith("ebur128=peak=true")
    assert args[args.index("-user_agent") + 1] == "UA/1"
    assert "-user_agent" not in loudness.ffmpeg_args("https://example/x")


class Clock:
    def __init__(self):
        self.now = 1000.0

    def __call__(self):
        return self.now


def test_at_most_two_measurements_start_a_minute():
    clock = Clock()
    book = loudness.LoudnessBook(per_minute=2, clock=clock)
    assert book.begin("a")
    book.finish("a", None)
    assert book.begin("b")
    book.finish("b", None)
    assert not book.begin("c")
    clock.now += 59
    assert not book.begin("c")
    clock.now += 1
    assert book.begin("c")


def test_a_video_is_never_measured_twice_at_once_or_again_once_known():
    clock = Clock()
    book = loudness.LoudnessBook(per_minute=10, clock=clock)
    assert book.begin("a")
    assert not book.begin("a")
    book.finish("a", {"track_gain": -10.0, "track_peak": 1.1})
    assert not book.begin("a")
    assert book.get("a") == {"track_gain": -10.0, "track_peak": 1.1}


def test_a_failed_measurement_lets_the_next_play_try_again():
    book = loudness.LoudnessBook(per_minute=10, clock=Clock())
    assert book.begin("a")
    book.finish("a", None)
    assert book.get("a") is None
    assert book.begin("a")


def test_the_book_forgets_the_oldest_past_its_size():
    book = loudness.LoudnessBook(per_minute=10, keep=2, clock=Clock())
    for video in ("a", "b", "c"):
        book.begin(video)
        book.finish(video, {"track_gain": 1.0, "track_peak": None})
    assert book.get("a") is None and book.get("c") is not None


app = pytest.importorskip("app")


def test_a_measurement_lands_in_facts(monkeypatch):
    monkeypatch.setattr(app, "_LOUDNESS", loudness.LoudnessBook(clock=Clock()))
    monkeypatch.setattr(app, "_measure_loudness", lambda url: {"track_gain": -10.0, "track_peak": 1.135011})
    started = []

    class InlineThread:
        def __init__(self, target, name, daemon):
            self.target = target

        def start(self):
            started.append(True)
            self.target()

    monkeypatch.setattr(app.threading, "Thread", InlineThread)
    assert app._maybe_measure_loudness("vidMeasured", "https://example/x")
    assert started
    # Known now: a second play measures nothing.
    assert not app._maybe_measure_loudness("vidMeasured", "https://example/x")

    app._facts_put("vidMeasured", {"itag": "140", "size": 6755585, "ext": "m4a", "abr": 129})
    answer = app.app.test_client().get("/facts?id=vidMeasured").get_json()
    assert answer["track_gain"] == -10.0 and answer["track_peak"] == 1.135011


class FakeUpstream:
    status_code = 206
    headers = {"Content-Range": "bytes 0-1/12345", "Content-Length": "2"}

    def iter_content(self, chunk_size):
        yield b"ab"

    def close(self):
        pass


@pytest.mark.parametrize("range_header,measured", [(None, True), ("bytes=0-", True), ("bytes=4096-", False)])
def test_only_a_play_from_the_first_byte_measures(monkeypatch, range_header, measured):
    calls = []
    monkeypatch.setattr(app, "_resolve_url", lambda video_id, bg=False: "https://googlevideo.example/x")
    monkeypatch.setattr(app, "_open_upstream", lambda url, headers, video_id: FakeUpstream())
    monkeypatch.setattr(app, "_maybe_measure_loudness", lambda video_id, url: calls.append(video_id))
    headers = {"Range": range_header} if range_header else {}
    app.app.test_client().get("/stream?id=vid-play", headers=headers)
    assert bool(calls) == measured


def test_the_ffmpeg_run_reads_only_https_and_prints_only_the_summary():
    args = loudness.ffmpeg_args("https://example/x")
    at = args.index("-protocol_whitelist")
    assert args[at + 1] == "https,tls,tcp"
    assert at < args.index("-i")
    # Per-frame lines go to the verbose level, which ffmpeg does not print by default.
    assert "framelog=verbose" in args[args.index("-af") + 1]


def test_one_measurement_runs_at_a_time():
    book = loudness.LoudnessBook(per_minute=10, clock=Clock())
    assert book.begin("a")
    assert not book.begin("b")
    book.finish("a", None)
    assert book.begin("b")


class NoGate:
    def acquire(self, bg, timeout):
        pytest.fail("a measurement never takes a yt-dlp slot")

    def release(self, bg):
        pytest.fail("a measurement never takes a yt-dlp slot")


def test_a_measurement_never_takes_a_yt_dlp_slot(monkeypatch):
    monkeypatch.setattr(app, "_GATE", NoGate())
    monkeypatch.setattr(app, "_LOUDNESS", loudness.LoudnessBook(clock=Clock()))
    monkeypatch.setattr(app, "_measure_loudness", lambda url: {"track_gain": -1.0, "track_peak": None})

    class InlineThread:
        def __init__(self, target, name, daemon):
            self.target = target

        def start(self):
            self.target()

    monkeypatch.setattr(app.threading, "Thread", InlineThread)
    assert app._maybe_measure_loudness("vidNoGate01", "https://example/x")
    assert app._LOUDNESS.get("vidNoGate01") == {"track_gain": -1.0, "track_peak": None}
