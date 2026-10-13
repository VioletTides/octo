"""The facts of a resolved stream: yt-dlp's printed format line first, the URL's own
parameters when yt-dlp printed NA, and the bytes of a play over both."""
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import facts  # noqa: E402

URL_140 = (
    "https://rr1---sn-x.googlevideo.com/videoplayback?expire=1&itag=140&source=youtube"
    "&mime=audio%2Fmp4&clen=6755585&dur=417.381"
)
URL_251 = "https://rr1---sn-x.googlevideo.com/videoplayback?itag=251&mime=audio%2Fwebm&clen=7012345"


def test_the_printed_line_gives_the_format_and_the_exact_size():
    assert facts.parse_format_line("140|6755585|m4a|129.486") == {
        "itag": "140",
        "size": 6755585,
        "ext": "m4a",
        "abr": 129,
    }


def test_a_field_yt_dlp_did_not_know_is_none():
    assert facts.parse_format_line("251|NA|webm|NA") == {
        "itag": "251",
        "size": None,
        "ext": "webm",
        "abr": None,
    }
    assert facts.parse_format_line("garbage") == {"itag": None, "size": None, "ext": None, "abr": None}


def test_the_url_names_its_itag_size_and_container():
    assert facts.facts_from_url(URL_140) == {"itag": "140", "size": 6755585, "ext": "m4a", "abr": None}
    assert facts.facts_from_url(None)["size"] is None


def test_clen_is_only_the_fallback_when_yt_dlp_printed_no_size():
    # A printed size wins even where the URL says something else.
    assert facts.stream_facts("140|6755000|m4a|129.5", URL_140)["size"] == 6755000
    assert facts.stream_facts("251|NA|webm|NA", URL_251) == {
        "itag": "251",
        "size": 7012345,
        "ext": "webm",
        "abr": None,
    }
    assert facts.stream_facts(None, URL_140)["itag"] == "140"


def test_a_run_splits_into_its_json_its_format_line_and_its_url():
    out = '{"id": "abc", "title": "A | B | C | D"}\n140|6755585|m4a|129.486\n' + URL_140 + "\n"
    rest, format_line, url = facts.split_run(out)
    assert rest == ['{"id": "abc", "title": "A | B | C | D"}']
    assert format_line == "140|6755585|m4a|129.486"
    assert url == URL_140
    # A video with no formats prints NA for the URL.
    rest, format_line, url = facts.split_run('{"id": "x"}\nNA|NA|NA|NA\nNA\n')
    assert url is None and format_line == "NA|NA|NA|NA"


@pytest.mark.parametrize(
    "header,total",
    [("bytes 0-1/12345", 12345), ("bytes */12345", 12345), ("bytes 0-1/*", None), (None, None), ("/", None)],
)
def test_the_total_after_the_slash_of_a_content_range(header, total):
    assert facts.total_from_content_range(header) == total


app = pytest.importorskip("app")


def test_facts_keep_what_is_known_and_a_played_size_replaces_a_printed_one():
    app._facts_put("vid-keep", {"itag": "140", "size": 100, "ext": "m4a", "abr": 129})
    app._facts_put("vid-keep", {"size": 120, "itag": None})
    assert app._facts_get("vid-keep") == {"itag": "140", "size": 120, "ext": "m4a", "abr": 129}


def test_a_search_payload_carries_its_videos_facts():
    app._facts_put("vid-payload", {"itag": "251", "size": 7012345, "ext": "webm", "abr": 160})
    payload = app._with_facts({"video_id": "vid-payload", "title": "t"})
    assert payload["size"] == 7012345 and payload["itag"] == "251" and payload["abr"] == 160
    assert "size" not in app._with_facts({"video_id": "vid-unknown"})


def test_facts_answers_from_what_is_known_without_running_yt_dlp(monkeypatch):
    app._facts_put("vid-known01", {"itag": "140", "size": 6755585, "ext": "m4a", "abr": 129})
    monkeypatch.setattr(app, "_run", lambda *a, **k: pytest.fail("no yt-dlp run for a known video"))
    answer = app.app.test_client().get("/facts?id=vid-known01")
    assert answer.status_code == 200
    assert answer.get_json() == {"video_id": "vid-known01", "itag": "140", "size": 6755585, "ext": "m4a", "abr": 129}


def test_facts_resolves_an_unknown_video_with_the_format_line(monkeypatch):
    calls = []

    def run(args, **kwargs):
        calls.append(args)
        return "140|6755585|m4a|129.486\n" + URL_140 + "\n"

    monkeypatch.setattr(app, "_run", run)
    answer = app.app.test_client().get("/facts?id=vid-cold001&bg=1")
    assert answer.get_json()["size"] == 6755585
    assert "--print" in calls[0] and facts.FORMAT_PRINT in calls[0]
    assert app._url_cache_get("vid-cold001") == URL_140


def test_facts_is_502_when_nothing_resolves(monkeypatch):
    monkeypatch.setattr(app, "_run", lambda *a, **k: None)
    assert app.app.test_client().get("/facts?id=vid-nothing").status_code == 502


@pytest.mark.parametrize(
    "value,valid",
    [
        ("dQw4w9WgXcQ", True),
        ("a-b_c-d_e-f", True),
        ("dQw4w9WgXc", False),
        ("dQw4w9WgXcQQ", False),
        ("dQw4w9WgXc!", False),
        ("../../etc/x", False),
        ("", False),
        (None, False),
    ],
)
def test_a_video_id_is_eleven_letters_digits_dashes_or_underscores(value, valid):
    assert facts.is_video_id(value) is valid


def test_facts_refuses_an_id_that_is_not_a_video_id_without_running_yt_dlp(monkeypatch):
    monkeypatch.setattr(app, "_run", lambda *a, **k: pytest.fail("no yt-dlp run for a bad id"))
    for bad in ("short", "dQw4w9WgXcQ%20--exec", "ytsearch1:x"):
        answer = app.app.test_client().get(f"/facts?id={bad}")
        assert answer.status_code == 400, bad
