#!/usr/bin/env bash
# The files 14-health and 19-library-edits copy into
# the run's library with exec (mounted read-only in the octo container at /parity-planted).
# Checked in like fixtures/music, for the same reason (encoder bytes differ between ffmpeg
# builds, and Navidrome reports sizes). Regenerating means re-recording the baseline.
#
#   health/Kestrel Moor/Slate Coast (2019)/   FLAC, the planted findings
#     01 - Harbor Lights.flac   every tag; the lossless copy of a duplicate
#     02 - Tidewater.flac       no DATE                      -> noYear, filled from the album
#     03 - Gull Song.flac       no GENRE                     -> noGenre, filled from the album
#     Breakwater.flac           no TRACKNUMBER               -> noTrackNumber, looked up
#     05 - Spark.flac           0.5 s                        -> noLength
#     06 - Undertow.flac        MUSICBRAINZ_ALBUMID set      -> a second "Slate Coast" album: splitAlbums
#   health/Kestrel Moor/Harbor Lights (2018)/01 - Harbor Lights.mp3
#                               the lossy copy, on a one-song album of its own -> duplicates
#   edits/Aurora Vale/Northern Lights (2020)/01 - Café del Mar.mp3
#                               a second copy of song_flac1, disc 2, same file stem as the
#                               FLAC and its .lrc (19 removes it with copy=true)
# No file has an embedded picture, so every one is also in noCover.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
out="$here/planted"
rm -rf "$out"
mkdir -p "$out"

ff() { ffmpeg -nostdin -hide_banner -loglevel error -y "$@"; }
bitexact=(-fflags +bitexact -flags:a +bitexact -map_metadata -1)
flac=(-c:a flac -compression_level 5)
mp3=(-c:a libmp3lame -b:a 32k -id3v2_version 4 -write_xing 0)

# tone <file> <freq> <seconds> <codec args...> -- <key=value tags...>
tone() {
  local file="$1" freq="$2" secs="$3"; shift 3
  local codec=() tags=()
  while [[ "$1" != "--" ]]; do codec+=("$1"); shift; done
  shift
  for kv in "$@"; do tags+=(-metadata "$kv"); done
  mkdir -p "$(dirname "$file")"
  ff -f lavfi -i "sine=frequency=$freq:duration=$secs:sample_rate=22050" -ac 1 \
     "${bitexact[@]}" "${codec[@]}" "${tags[@]}" "$file"
}

h="$out/health/Kestrel Moor"
sc="$h/Slate Coast (2019)"
k=(artist="Kestrel Moor" album="Slate Coast" album_artist="Kestrel Moor" disc=1)
tone "$sc/01 - Harbor Lights.flac" 600 3   "${flac[@]}" -- "${k[@]}" date=2019 genre=Folk track=1 title="Harbor Lights"
tone "$sc/02 - Tidewater.flac"     660 3   "${flac[@]}" -- "${k[@]}"           genre=Folk track=2 title=Tidewater
tone "$sc/03 - Gull Song.flac"     700 2   "${flac[@]}" -- "${k[@]}" date=2019            track=3 title="Gull Song"
tone "$sc/Breakwater.flac"         740 3   "${flac[@]}" -- "${k[@]}" date=2019 genre=Folk         title=Breakwater
tone "$sc/05 - Spark.flac"         800 0.5 "${flac[@]}" -- "${k[@]}" date=2019 genre=Folk track=5 title=Spark
tone "$sc/06 - Undertow.flac"      880 2   "${flac[@]}" -- "${k[@]}" date=2019 genre=Folk track=6 title=Undertow \
     MUSICBRAINZ_ALBUMID=8c1f6a5e-0b6e-4c0e-9a51-6d1b2b7f4a01
tone "$h/Harbor Lights (2018)/01 - Harbor Lights.mp3" 600 3 "${mp3[@]}" -- \
     artist="Kestrel Moor" album="Harbor Lights" album_artist="Kestrel Moor" date=2018 genre=Folk track=1 disc=1 title="Harbor Lights"

e="$out/edits/Aurora Vale/Northern Lights (2020)"
tone "$e/01 - Café del Mar.mp3" 440 2 "${mp3[@]}" -- \
     artist="Aurora Vale" album="Northern Lights" album_artist="Aurora Vale" date=2020 genre=Electronic track=1 disc=2 title="Café del Mar"

# The corpus sets the mtimes itself after copying (touch -d @1577909045), so none are kept here.
( cd "$out" && find . -type f -print0 | sort -z | xargs -0 sha256sum ) > "$here/planted.sha256"
echo "Wrote $(find "$out" -type f | wc -l) files to $out"
