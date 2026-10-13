//! Whether a song already in the library is explicit, the clean edit or neither, for the
//! library-wide marking.
//!
//! Only a confident match counts:
//! 1. A title or file name that says clean is the clean edit, as for a download.
//! 2. The song's ISRC names one recording, and the catalog's track with that code says.
//! 3. Otherwise every catalog track that is the same song by artist, title (version included)
//!    and length, on the same album when any is: when they all say the same thing, that is the
//!    answer. A clean and an explicit copy that both fit, or nothing that fits, is "unsure" and
//!    nothing is written.

use async_trait::async_trait;
use indexmap::IndexSet;

use crate::common::dotnet::is_blank;
use crate::common::song_identity::{SongIdentity, SongRef};
use crate::metadata::deezer_metadata_service::{CatalogTrackAnswer, CatalogTrackFacts};
use crate::models::domain::explicit_status;
use crate::tagging::explicit_advisory;

/// What a library song's own file says about it, for the explicit lookup.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LibrarySongFacts {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub seconds: Option<f64>,
    pub isrcs: Vec<String>,
    pub file_name: Option<String>,
}

/// What the lookup found about one library song's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplicitOutcome {
    Explicit,
    Clean,
    NotExplicit,
    /// No match confident enough to write anything.
    Unsure,
}

/// The finding, in words for the dashboard, and the catalog track that said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitFinding {
    pub outcome: ExplicitOutcome,
    pub how: String,
    pub catalog_track_id: Option<String>,
    /// The catalog did not answer this time; ask again later rather than record it.
    pub catalog_busy: bool,
}

impl ExplicitFinding {
    pub fn new(outcome: ExplicitOutcome, how: impl Into<String>, catalog_track_id: Option<String>) -> Self {
        Self {
            outcome,
            how: how.into(),
            catalog_track_id,
            catalog_busy: false,
        }
    }

    pub fn busy() -> Self {
        Self {
            catalog_busy: true,
            ..Self::new(ExplicitOutcome::Unsure, "the catalog did not answer", None)
        }
    }

    /// The advisory tag's number: 1 explicit, 2 clean, 0 neither; None when unsure.
    pub fn advisory(&self) -> Option<i32> {
        match self.outcome {
            ExplicitOutcome::Explicit => Some(explicit_advisory::EXPLICIT),
            ExplicitOutcome::Clean => Some(explicit_advisory::CLEAN),
            ExplicitOutcome::NotExplicit => Some(explicit_advisory::NONE),
            ExplicitOutcome::Unsure => None,
        }
    }
}

/// Where the lookup asks. Deezer in the app; a stand-in in tests.
#[async_trait]
pub trait IExplicitCatalog: Send + Sync {
    async fn by_isrc(&self, isrc: &str) -> CatalogTrackAnswer;
    async fn search(&self, artist: Option<&str>, title: Option<&str>) -> CatalogTrackAnswer;
}

pub async fn find(song: &LibrarySongFacts, catalog: &dyn IExplicitCatalog) -> ExplicitFinding {
    if says_clean(song.title.as_deref()) || says_clean(song.file_name.as_deref()) {
        return ExplicitFinding::new(ExplicitOutcome::Clean, "its name says it is the clean edit", None);
    }

    let codes: IndexSet<String> = song
        .isrcs
        .iter()
        .filter_map(|isrc| SongIdentity::normalize_isrc(isrc))
        .collect();
    for isrc in codes {
        let answer = catalog.by_isrc(&isrc).await;
        if answer.did_not_answer {
            return ExplicitFinding::busy();
        }
        if let Some(coded) = answer
            .tracks
            .iter()
            .find(|track| track.explicit_content.is_some())
        {
            return ExplicitFinding::new(
                outcome(coded.explicit_content),
                format!("same ISRC ({isrc})"),
                Some(coded.track_id.clone()),
            );
        }
    }

    let (Some(artist), Some(title)) = (
        song.artist.as_deref().filter(|a| !is_blank(a)),
        song.title.as_deref().filter(|t| !is_blank(t)),
    ) else {
        return ExplicitFinding::new(ExplicitOutcome::Unsure, "the file names no artist or title", None);
    };
    let found = catalog.search(Some(artist), Some(title)).await;
    if found.did_not_answer {
        return ExplicitFinding::busy();
    }
    decide(song, &found.tracks)
}

/// The name rule over the tracks a search found.
pub fn decide(song: &LibrarySongFacts, tracks: &[CatalogTrackFacts]) -> ExplicitFinding {
    let mine = SongRef {
        title: song.title.clone().unwrap_or_default(),
        artist: song.artist.clone().unwrap_or_default(),
        seconds: song.seconds,
        isrcs: song.isrcs.clone(),
    };
    let same: Vec<&CatalogTrackFacts> = tracks
        .iter()
        .filter(|track| {
            let theirs = SongRef {
                title: track.title.clone(),
                artist: track.artist.clone(),
                seconds: track.duration.map(f64::from),
                isrcs: track.isrc.iter().cloned().collect(),
            };
            SongIdentity::same(&mine, &theirs, None).is_same()
        })
        .collect();
    if same.is_empty() {
        return ExplicitFinding::new(
            ExplicitOutcome::Unsure,
            "no catalog song matched closely enough",
            None,
        );
    }

    let on_album: Vec<&CatalogTrackFacts> = same
        .iter()
        .copied()
        .filter(|track| albums_agree(song.album.as_deref(), track.album.as_deref()))
        .collect();
    let pool = if on_album.is_empty() { &same } else { &on_album };
    let said: Vec<&CatalogTrackFacts> = pool
        .iter()
        .copied()
        .filter(|track| track.explicit_content.is_some())
        .collect();
    if said.is_empty() {
        return ExplicitFinding::new(ExplicitOutcome::Unsure, "the catalog does not say", None);
    }

    let mut answers: Vec<ExplicitOutcome> = Vec::new();
    for track in &said {
        let answer = outcome(track.explicit_content);
        if !answers.contains(&answer) {
            answers.push(answer);
        }
    }
    if answers.len() > 1 {
        let why = if answers.contains(&ExplicitOutcome::Clean) && answers.contains(&ExplicitOutcome::Explicit)
        {
            "an explicit and a clean copy both fit"
        } else {
            "the copies that fit disagree"
        };
        return ExplicitFinding::new(ExplicitOutcome::Unsure, why, None);
    }

    let place = if on_album.is_empty() {
        "artist, title and length"
    } else {
        "artist, title, length and album"
    };
    let copies = if said.len() == 1 {
        String::new()
    } else {
        format!(", {} copies agree", said.len())
    };
    ExplicitFinding::new(
        answers[0],
        format!("{place}{copies}"),
        Some(said[0].track_id.clone()),
    )
}

fn outcome(content: Option<i32>) -> ExplicitOutcome {
    match content {
        Some(explicit_status::EXPLICIT) => ExplicitOutcome::Explicit,
        Some(explicit_status::CLEAN) => ExplicitOutcome::Clean,
        _ => ExplicitOutcome::NotExplicit,
    }
}

fn albums_agree(a: Option<&str>, b: Option<&str>) -> bool {
    let x = SongIdentity::key(a.unwrap_or(""));
    let y = SongIdentity::key(b.unwrap_or(""));
    !x.is_empty() && !y.is_empty() && (x == y || x.contains(&y) || y.contains(&x))
}

fn says_clean(title: Option<&str>) -> bool {
    title.is_some_and(|title| {
        !is_blank(title)
            && SongIdentity::parse_title(title, None)
                .versions
                .iter()
                .any(|version| version == "clean")
    })
}

#[cfg(test)]
#[path = "explicit_lookup_tests.rs"]
mod tests;
