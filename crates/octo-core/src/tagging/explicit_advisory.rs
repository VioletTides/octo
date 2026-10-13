//! Whether the version that landed is the explicit one or the clean edit, for the advisory tag
//! the library server reads into each song's explicit status. A clean and an explicit copy of a
//! song carry different codes, so the catalog's flag counts only from the hit whose code is the
//! landed file's, or from a hit the chooser was sure of. The code that was asked for stands in
//! only when the file and the fingerprint name none, and a name that says clean beats it: the
//! file a peer shared may be the other version.

use indexmap::IndexSet;

use crate::common::song_identity::SongIdentity;
use crate::models::domain::song::Song;
use crate::tagging::tag_evidence::TagSource;
use crate::tagging::tag_plan::{FieldDecision, TagConfidence, TagPlan};

/// The advisory value for one download and what said so, for the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisoryDecision {
    pub value: i32,
    pub source: String,
}

impl AdvisoryDecision {
    pub fn new(value: i32, source: impl Into<String>) -> Self {
        Self {
            value,
            source: source.into(),
        }
    }
}

pub const NONE: i32 = 0;
pub const EXPLICIT: i32 = 1;
pub const CLEAN: i32 = 2;

/// The source a decision names when the file's own name said clean.
pub const FILE_NAME_SOURCE: &str = "the file name";

/// The advisory for this download, or None when nothing that matched this exact version said.
/// `source_file` is the peer's file name, when there was a peer.
pub fn decide(song: &Song, plan: Option<&TagPlan>, source_file: Option<&str>) -> Option<AdvisoryDecision> {
    // A name that says clean is the strongest evidence there is: it is what was shared.
    let file_title = plan
        .and_then(|plan| plan.evidence.as_ref())
        .and_then(|evidence| evidence.file.title.as_deref());
    if says_clean(Some(&song.title))
        || says_clean(file_title)
        || says_clean(leaf_name(source_file).as_deref())
    {
        return Some(AdvisoryDecision::new(CLEAN, FILE_NAME_SOURCE));
    }
    let plan = plan?;

    let catalog: Vec<_> = plan
        .ranked
        .iter()
        .map(|scored| &scored.candidate)
        .filter(|candidate| candidate.source == TagSource::Catalog)
        .collect();
    if catalog.is_empty() {
        return None;
    }

    let landed = landed_codes(song, plan);
    if !landed.is_empty() {
        let same_code = catalog
            .iter()
            .find(|candidate| SongIdentity::shares_isrc(&landed, &candidate.isrcs))?;
        return from_catalog(same_code.explicit_content, same_code.explicit)
            .map(|value| AdvisoryDecision::new(value, TagSource::Catalog.name()));
    }

    // Without a code to compare, only a catalog hit the chooser settled on outright.
    if !plan.rehearsed
        && plan.confidence == TagConfidence::Strong
        && let Some(chosen) = plan
            .chosen
            .as_ref()
            .map(|chosen| &chosen.candidate)
            .filter(|candidate| candidate.source == TagSource::Catalog)
        && let Some(value) = from_catalog(chosen.explicit_content, chosen.explicit)
    {
        return Some(AdvisoryDecision::new(value, TagSource::Catalog.name()));
    }
    None
}

/// The advisory as the tag report shows it.
pub fn field(decision: &AdvisoryDecision) -> FieldDecision {
    let value = match decision.value {
        EXPLICIT => "explicit",
        CLEAN => "clean edit",
        _ => "not explicit",
    };
    FieldDecision::new(Some(value), Some(&decision.source))
}

/// The catalog's numbers as the advisory tag holds them: its 1 is explicit and its 3 the clean
/// edit; 0 is neither. Its other values say it does not know, so only the plain explicit flag
/// can still answer.
pub fn from_catalog(explicit_content: Option<i32>, explicit_lyrics: Option<bool>) -> Option<i32> {
    match explicit_content {
        Some(1) => Some(EXPLICIT),
        Some(3) => Some(CLEAN),
        Some(0) => Some(NONE),
        _ => (explicit_lyrics == Some(true)).then_some(EXPLICIT),
    }
}

/// The codes of what landed: the file's own, then the recording the fingerprint settled, then
/// the song's (the request's or the match's) when nothing else named one.
fn landed_codes(song: &Song, plan: &TagPlan) -> IndexSet<String> {
    if let Some(file) = plan
        .evidence
        .as_ref()
        .map(|evidence| &evidence.file.isrcs)
        .filter(|isrcs| !isrcs.is_empty())
    {
        return SongIdentity::isrcs(file);
    }
    if plan.recording_confirmed()
        && let Some(confirmed) = plan
            .chosen
            .as_ref()
            .filter(|chosen| !chosen.candidate.isrcs.is_empty())
    {
        return SongIdentity::isrcs(&confirmed.candidate.isrcs);
    }
    SongIdentity::isrcs(song.isrc.as_deref())
}

fn says_clean(title: Option<&str>) -> bool {
    title.is_some_and(|title| {
        !crate::common::dotnet::is_blank(title)
            && SongIdentity::parse_title(title, None)
                .versions
                .iter()
                .any(|version| version == "clean")
    })
}

/// The last part of a peer's path, either separator, without its extension (cut at its last
/// dot).
fn leaf_name(path: Option<&str>) -> Option<String> {
    let path = path.filter(|path| !crate::common::dotnet::is_blank(path))?;
    let leaf = path.split(['\\', '/']).rfind(|part| !part.is_empty())?;
    Some(match leaf.rfind('.') {
        Some(dot) => leaf[..dot].to_string(),
        None => leaf.to_string(),
    })
}

/// The advisory half of the release fact tags ("which version landed").
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tagging::tag_evidence::{FileFacts, ReleaseCandidate, TagEvidence, TagRequest};
    use crate::tagging::tag_plan::ScoredCandidate;

    #[test]
    fn from_catalog_maps_the_catalogs_numbers() {
        let cases = [
            (Some(1), None, Some(EXPLICIT)),
            (Some(3), Some(false), Some(CLEAN)),
            (Some(0), Some(false), Some(NONE)),
            (Some(2), Some(true), Some(EXPLICIT)),
            (Some(6), Some(false), None),
            (None, None, None),
        ];
        for (content, explicit_lyrics, expected) in cases {
            assert_eq!(
                from_catalog(content, explicit_lyrics),
                expected,
                "{content:?} {explicit_lyrics:?}"
            );
        }
    }

    fn hit(isrc: &str, content: i32) -> ReleaseCandidate {
        ReleaseCandidate {
            isrcs: vec![isrc.to_string()],
            explicit_content: Some(content),
            release_title: Some("good kid, m.A.A.d city".into()),
            ..ReleaseCandidate::new(TagSource::Catalog, "Money Trees", "Kendrick Lamar")
        }
    }

    fn plan(file_isrcs: &[&str], confidence: TagConfidence, hits: Vec<ReleaseCandidate>) -> TagPlan {
        let request = TagRequest {
            artist: "Kendrick Lamar".into(),
            title: "Money Trees".into(),
            duration_seconds: Some(386),
            ..Default::default()
        };
        let file = FileFacts {
            isrcs: file_isrcs.iter().map(|isrc| isrc.to_string()).collect(),
            tags_are_evidence: !file_isrcs.is_empty(),
            ..FileFacts::unknown(".flac")
        };
        let ranked: Vec<ScoredCandidate> = hits
            .into_iter()
            .map(|candidate| ScoredCandidate {
                candidate,
                distance: 0.1,
                breakdown: Vec::new(),
            })
            .collect();
        TagPlan {
            confidence,
            chosen: ranked.first().cloned(),
            ranked,
            evidence: Some(TagEvidence {
                request,
                file,
                fingerprint_threshold: 0.8,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn song(title: &str, isrc: Option<&str>) -> Song {
        Song {
            title: title.into(),
            isrc: isrc.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn the_hit_with_the_landed_code_decides_even_when_it_is_not_the_first() {
        let plan = plan(
            &["USUM71210787"],
            TagConfidence::Medium,
            vec![hit("USUM71210782", 1), hit("USUM71210787", 3)],
        );
        let decision = decide(&song("Money Trees", Some("USUM71210782")), Some(&plan), None);
        assert_eq!(decision.expect("a decision").value, CLEAN);
    }

    #[test]
    fn no_code_to_compare_only_a_strong_catalog_match_decides() {
        let money_trees = song("Money Trees", None);
        assert_eq!(
            decide(
                &money_trees,
                Some(&plan(&[], TagConfidence::Medium, vec![hit("USUM71210782", 1)])),
                None
            ),
            None
        );
        assert_eq!(
            decide(
                &money_trees,
                Some(&plan(&[], TagConfidence::Strong, vec![hit("USUM71210782", 1)])),
                None
            )
            .expect("a decision")
            .value,
            EXPLICIT
        );

        let mut rehearsed = plan(&[], TagConfidence::Strong, vec![hit("USUM71210782", 1)]);
        rehearsed.rehearsed = true;
        assert_eq!(decide(&money_trees, Some(&rehearsed), None), None);
    }

    #[test]
    fn a_code_no_hit_carries_says_nothing() {
        let plan = plan(
            &["GBAAA9800001"],
            TagConfidence::Strong,
            vec![hit("USUM71210782", 1)],
        );
        assert_eq!(decide(&song("Money Trees", None), Some(&plan), None), None);
    }

    #[test]
    fn a_name_that_says_clean_is_the_clean_edit() {
        let cases = [
            ("Money Trees (Clean)", None),
            (
                "Money Trees",
                Some(r"@@peer\Music\Kendrick Lamar - Money Trees (Clean Version).flac"),
            ),
        ];
        for (title, source_file) in cases {
            let plan = plan(&[], TagConfidence::Strong, vec![hit("USUM71210782", 1)]);
            assert_eq!(
                decide(&song(title, None), Some(&plan), source_file),
                Some(AdvisoryDecision::new(CLEAN, "the file name")),
                "{title}"
            );
        }
    }

    /// The report words (`field`), and a decision with no plan at all.
    #[test]
    fn the_report_words_and_no_plan() {
        let words = |value| field(&AdvisoryDecision::new(value, "Catalog")).value;
        assert_eq!(words(EXPLICIT).as_deref(), Some("explicit"));
        assert_eq!(words(CLEAN).as_deref(), Some("clean edit"));
        assert_eq!(words(NONE).as_deref(), Some("not explicit"));
        assert_eq!(decide(&song("Money Trees", None), None, None), None);
        assert_eq!(
            decide(
                &song("Money Trees", None),
                None,
                Some("a/b/Money Trees (Clean).mp3")
            )
            .map(|d| d.source),
            Some(FILE_NAME_SOURCE.to_string())
        );
    }
}
