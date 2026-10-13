#!/usr/bin/env python3
"""Build the binary fixtures the import corpus (corpus/12-imports.json) uploads.

Writes parity/fixtures/imports/ (run from anywhere; stdlib only). Every zip gets fixed entry
names, a fixed order and a fixed timestamp (2020-01-01 00:00:00), so a rebuild with the same
Python and zlib gives the same bytes. Deflate output can differ between zlib builds, so the
files are meant to be checked in like the music library, and a rebuild means re-recording.

    spotify-export.zip         Spotify's "Download your data" layout: Playlist1.json and
                               YourLibrary.json under "Spotify Account Data/", plus two files
                               Octo ignores. Stored (no compression).
    not-a-zip.zip              text bytes with a .zip name: the zip reader refuses it.
    too-many-files.zip         201 empty entries: past the 200-file cap. Stored, ~25 KB.
    too-many-songs.zip         two CSVs of 25,001 songs each: past 50,000 across one file.
                               Deflated, ~2 KB.
    too-many-songs-one.zip     one CSV of 50,001 songs: the CSV reader's own cap. ~2 KB.
    too-big-unpacked.zip       five Playlist<n>.json of 54,000,000 bytes each (270,000,000 in
                               all, past 256 MiB = 268,435,456; each under the 64 MiB entry
                               limit). Deflated level 9, ~270 KB. Octo reads ~268 MB to refuse it.
    501-lists.csv              one CSV with a Playlist column naming 501 lists: past 500 lists.
    too-many-songs.csv         OPTIONAL plain CSV of 50,001 songs (~200 KB), the same cap as
                               too-many-songs-one.zip without a zip around it.
"""
from __future__ import annotations

import io
import os
import sys
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
# Default target: parity/fixtures/imports next to this script once it lives in parity/fixtures/.
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "imports")
STAMP = (2020, 1, 1, 0, 0, 0)

YOUR_LIBRARY = (
    '{"tracks":['
    '{"artist":"Aurora Vale","album":"Northern Lights","track":"Polar Night","uri":"spotify:track:parity0000000000000001"},'
    '{"artist":"Zephyr Echo","album":"Glass Harbor","track":"Glass Harbor","uri":"spotify:track:parity0000000000000004"},'
    '{"artist":"","album":"","track":"No Artist","uri":""}'
    '],"albums":[],"shows":[],"episodes":[],"bannedTracks":[],"artists":[],"bannedArtists":[],"other":[]}'
)

# The zip's Playlist1.json holds one song more than the standalone one (Hjarta), so the corpus
# sees the list it replaces change.
PLAYLIST1_ZIP = (
    '{"playlists":['
    '{"name":"Parity Export One","lastModifiedDate":"2026-01-01","items":['
    '{"track":{"trackName":"Midnight Sun","artistName":"Aurora Vale","albumName":"Northern Lights","trackUri":"spotify:track:parity0000000000000061"},"episode":null,"localTrack":null,"addedDate":"2026-01-01"},'
    '{"track":null,"episode":{"episodeName":"Parity Episode","showName":"Parity Show"},"localTrack":null,"addedDate":"2026-01-01"},'
    '{"track":{"trackName":"First Love","artistName":"宇多田テスト","albumName":"初恋テスト","trackUri":"spotify:track:parity0000000000000062"},"episode":null,"localTrack":null,"addedDate":"2026-01-01"},'
    '{"track":{"trackName":"Hjarta","artistName":"Bjørn Ålund","albumName":"Ágætis Prófun","trackUri":"spotify:track:parity0000000000000063"},"episode":null,"localTrack":null,"addedDate":"2026-01-02"}'
    '],"description":null,"numberOfFollowers":0},'
    '{"name":"Empty One","lastModifiedDate":"2026-01-01","items":[],"description":null,"numberOfFollowers":0}'
    ']}'
)


def entry(name: str, compress: int) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(name, date_time=STAMP)
    info.compress_type = compress
    info.external_attr = 0o644 << 16
    info.create_system = 3  # unix, so the bytes do not depend on the OS that builds it
    return info


def write_zip(path: str, files: list[tuple[str, bytes]], compress: int = zipfile.ZIP_STORED, level: int | None = None) -> None:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", compression=compress, compresslevel=level) as zf:
        for name, data in files:
            zf.writestr(entry(name, compress), data, compress_type=compress, compresslevel=level)
    with open(path, "wb") as fh:
        fh.write(buf.getvalue())
    print(f"{os.path.basename(path)}: {len(buf.getvalue())} bytes")


def rows_csv(count: int) -> bytes:
    return ("Track Name,Artist Name(s)\n" + "x,y\n" * count).encode()


def main() -> None:
    os.makedirs(OUT, exist_ok=True)
    d = "Spotify Account Data/"
    write_zip(os.path.join(OUT, "spotify-export.zip"), [
        (d + "Playlist1.json", PLAYLIST1_ZIP.encode()),
        (d + "ReadMeFirst_YourSpotifyData.pdf", b"not really a pdf\n"),
        (d + "Userdata.json", b'{"username":"parityuser","country":"US"}'),
        (d + "YourLibrary.json", YOUR_LIBRARY.encode()),
    ])
    with open(os.path.join(OUT, "not-a-zip.zip"), "wb") as fh:
        fh.write(b"this is not a zip file\n")
    write_zip(os.path.join(OUT, "too-many-files.zip"),
              [(f"junk{i:03d}.txt", b"") for i in range(201)])
    write_zip(os.path.join(OUT, "too-many-songs.zip"),
              [("a.csv", rows_csv(25_001)), ("b.csv", rows_csv(25_001))], zipfile.ZIP_DEFLATED, 9)
    write_zip(os.path.join(OUT, "too-many-songs-one.zip"),
              [("everything.csv", rows_csv(50_001))], zipfile.ZIP_DEFLATED, 9)
    # JSON padding: JsonDocument.Parse is cheap on it and the root has no tracks or playlists,
    # so the first four entries read as nothing and the fifth trips the unpacked budget.
    pad = 54_000_000
    body = b'{"pad":"' + b"a" * (pad - 10) + b'"}'
    assert len(body) == pad
    write_zip(os.path.join(OUT, "too-big-unpacked.zip"),
              [(f"Playlist{i}.json", body) for i in range(1, 6)], zipfile.ZIP_DEFLATED, 9)
    lists = "Track Name,Artist Name(s),Playlist Name\n" + "".join(
        f"Song {i:03d},Parity Many,List {i:03d}\n" for i in range(1, 502))
    with open(os.path.join(OUT, "501-lists.csv"), "wb") as fh:
        fh.write(lists.encode())
    with open(os.path.join(OUT, "too-many-songs.csv"), "wb") as fh:
        fh.write(rows_csv(50_001))
    print("done:", OUT)


if __name__ == "__main__":
    main()
