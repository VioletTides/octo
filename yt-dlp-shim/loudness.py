"""ReplayGain for streamed videos: EBU R128 loudness, measured with ffmpeg on play.

Measuring reads the whole audio again, so it happens only when a video is played (never on a
search or a resolve), at most a few times a minute, one measurement at a time, and never again
once known. Kept free of any app.py import so it stays cheap to unit test.
"""
from __future__ import annotations

import re
import threading
import time
from collections import OrderedDict, deque
from typing import Callable, Optional

# ReplayGain 2.0 plays every track at -18 LUFS.
REFERENCE_LUFS = -18.0

_INTEGRATED = re.compile(r"Integrated loudness:\s*\n\s*I:\s*(-?\d+(?:\.\d+)?)\s*LUFS")
_TRUE_PEAK = re.compile(r"True peak:\s*\n\s*Peak:\s*(-?\d+(?:\.\d+)?|-inf)\s*dBFS")


def ffmpeg_args(url: str, user_agent: str = "") -> list[str]:
    """The ffmpeg run that measures a stream's integrated loudness and true peak.

    It reads only over HTTPS, and the filter's per-frame lines go to the verbose level, which
    ffmpeg does not print, so the output is the summary alone.
    """
    args = ["ffmpeg", "-nostats", "-hide_banner", "-nostdin", "-protocol_whitelist", "https,tls,tcp"]
    if user_agent:
        args += ["-user_agent", user_agent]
    return args + ["-i", url, "-vn", "-af", "ebur128=peak=true:framelog=verbose", "-f", "null", "-"]


def parse_ebur128(output: str) -> Optional[tuple[float, Optional[float]]]:
    """(integrated LUFS, true peak dBFS) from the ebur128 filter's summary, or None without one.

    The summary is the last block ffmpeg prints, so the last match wins over any progress lines.
    """
    integrated = _INTEGRATED.findall(output or "")
    if not integrated:
        return None
    peaks = _TRUE_PEAK.findall(output or "")
    peak = None
    if peaks and peaks[-1] != "-inf":
        peak = float(peaks[-1])
    return float(integrated[-1]), peak


def replay_gain(integrated_lufs: float, true_peak_dbfs: Optional[float]) -> dict:
    """ReplayGain's track gain (dB to reach the reference) and track peak (linear, 1.0 = full scale)."""
    gain = round(REFERENCE_LUFS - integrated_lufs, 2)
    peak = None if true_peak_dbfs is None else round(10 ** (true_peak_dbfs / 20.0), 6)
    return {"track_gain": gain, "track_peak": peak}


class LoudnessBook:
    """What is known, what is being measured, and how many measurements started this minute.

    One measurement runs at a time: its own limit, apart from the yt-dlp gate, so measuring never
    takes a slot a play or a resolve needs.
    """

    def __init__(self, per_minute: int = 2, keep: int = 4096, clock: Callable[[], float] = time.monotonic):
        self.per_minute = max(1, per_minute)
        self.keep = max(1, keep)
        self._clock = clock
        self._lock = threading.Lock()
        self._known: "OrderedDict[str, dict]" = OrderedDict()
        self._running: set[str] = set()
        self._starts: deque[float] = deque()

    def get(self, video_id: str) -> Optional[dict]:
        with self._lock:
            known = self._known.get(video_id)
            if known is not None:
                self._known.move_to_end(video_id)
            return dict(known) if known is not None else None

    def begin(self, video_id: str) -> bool:
        """Whether to measure `video_id` now: not known, no measurement running, and the minute
        has room. True counts the measurement against the minute and marks it running."""
        now = self._clock()
        with self._lock:
            if not video_id or video_id in self._known or self._running:
                return False
            while self._starts and now - self._starts[0] >= 60.0:
                self._starts.popleft()
            if len(self._starts) >= self.per_minute:
                return False
            self._starts.append(now)
            self._running.add(video_id)
            return True

    def finish(self, video_id: str, measured: Optional[dict]) -> None:
        """Ends a measurement; a result is kept, a failure lets the next play try again."""
        with self._lock:
            self._running.discard(video_id)
            if measured and measured.get("track_gain") is not None:
                self._known[video_id] = dict(measured)
                self._known.move_to_end(video_id)
                while len(self._known) > self.keep:
                    self._known.popitem(last=False)
