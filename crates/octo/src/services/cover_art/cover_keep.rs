//! Whether the cover a song already has is worth keeping.
//!
//! Whether the cover a song arrived with (or the cover of the library copy a replacement takes
//! the place of) is worth keeping. Asked before the cover chain replaces it: the chain used to
//! put Apple's master over every file, so a peer's clean 1400 px scan of the same art was thrown
//! away for no gain, while a peer's 300 px thumbnail or another album's picture is exactly what
//! the chain is for.
//!
//! The rules, in order:
//! 1. Unreadable or under 150 px: replace.
//! 2. Not square, when video covers are being replaced: replace.
//! 3. Art of the album the file was filed away from: replace.
//! 4. A different picture from the album's cover (the look-alike hash more than
//!    `LIKENESS_TOLERANCE` bits apart): replace. The library copy's own cover is spared this
//!    one: it is what the album shows now, and the cover art page is where that picture gets
//!    changed.
//! 5. The same art blown up from a smaller picture (less than [`BLOWN_UP_RATIO`] of the album
//!    cover's fine detail at the same size): replace.
//! 6. The same art at [`KEEP_SIDE`] px or more: keep.
//! 7. Anything else is weighed: it stays unless the chain finds one at least [`MUCH_LARGER`]
//!    times its side.

use octo_media::cover::cover_image;

use super::DownloadCoverResolver;

/// What to do with a cover a song already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnCoverVerdict {
    /// Good enough to keep without asking anyone for another.
    Keep,
    /// Worth keeping unless the cover sources find one much larger, or (when nothing said what
    /// the album's cover looks like) one that shows it is another picture.
    Weigh,
    /// Not worth keeping when anything else is found.
    Replace,
}

/// The verdict on one cover, why, and whether it looked like the album's cover (None when there
/// was nothing to compare it with).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnCoverJudgement {
    pub verdict: OwnCoverVerdict,
    pub why: String,
    pub same_art: Option<bool>,
    pub width: u32,
    pub height: u32,
}

impl OwnCoverJudgement {
    fn new(verdict: OwnCoverVerdict, why: &str, same_art: Option<bool>, width: u32, height: u32) -> Self {
        Self {
            verdict,
            why: why.to_string(),
            same_art,
            width,
            height,
        }
    }

    pub fn side(&self) -> u32 {
        self.width.min(self.height)
    }
}

/// Big enough to keep outright when it is the same art: the catalog's own covers are 1000 px,
/// and the chain stops looking at that size too.
pub const KEEP_SIDE: u32 = DownloadCoverResolver::SHARP_SIDE;

/// How much larger a found cover has to be to replace one that is the same art but under
/// [`KEEP_SIDE`]: 800 px gives way to the catalog's 1000, 900 px does not. The cover art page
/// asks a little less (1.2) because there a person picked the album.
pub const MUCH_LARGER: f64 = 1.25;

/// A cover with less than this share of the album cover's fine detail, measured at the same
/// size, was blown up from a smaller picture.
pub const BLOWN_UP_RATIO: f64 = 0.35;

/// Below this much detail the album's own cover is too plain (a flat color, a single word) for
/// the comparison to say anything.
pub(crate) const MIN_REFERENCE_DETAIL: f64 = 1.0;

/// The detail comparison runs at no more than this side, to stay quick.
const DETAIL_SIDE: u32 = 800;

/// Judges `own` against `reference`, the album's cover as a source has it (the catalog's, or
/// failing that the one the chain found), or None when there is none to compare with.
pub fn judge(
    own: &[u8],
    reference: Option<&[u8]>,
    require_square: bool,
    other_album: bool,
    library_copy: bool,
) -> OwnCoverJudgement {
    use OwnCoverVerdict::{Keep, Replace, Weigh};
    let Some((width, height)) = cover_image::measure(own) else {
        return OwnCoverJudgement::new(Replace, "it could not be read", None, 0, 0);
    };
    if !cover_image::is_usable(Some(own), false) {
        return OwnCoverJudgement::new(Replace, "it was too small", None, width, height);
    }
    if require_square && !cover_image::is_usable(Some(own), true) {
        return OwnCoverJudgement::new(Replace, "it was not square", None, width, height);
    }
    if other_album {
        return OwnCoverJudgement::new(
            Replace,
            "it was the cover of the album the file was filed under before",
            None,
            width,
            height,
        );
    }

    let side = width.min(height);
    let reference = reference.filter(|bytes| !bytes.is_empty());
    let mut same_art = None;
    if let Some(reference) = reference
        && cover_image::is_usable(Some(reference), true)
        && let Some(own_looks) = cover_image::looks_hash(Some(own))
        && let Some(reference_looks) = cover_image::looks_hash(Some(reference))
    {
        same_art = Some(cover_image::look_alike(own_looks, reference_looks));
    }

    if same_art == Some(false) && !library_copy {
        return OwnCoverJudgement::new(
            Replace,
            "it was a different picture from the album's cover",
            Some(false),
            width,
            height,
        );
    }
    if same_art == Some(true)
        && let Some(reference) = reference
        && blown_up(own, side, reference)
    {
        return OwnCoverJudgement::new(
            Replace,
            "it was a smaller picture blown up",
            Some(true),
            width,
            height,
        );
    }
    if side >= KEEP_SIDE && (same_art == Some(true) || library_copy) {
        let why = if same_art == Some(true) {
            "the same art"
        } else {
            "the cover your library shows now"
        };
        return OwnCoverJudgement::new(Keep, why, same_art, width, height);
    }
    let why = if same_art == Some(true) {
        "the same art"
    } else {
        ""
    };
    OwnCoverJudgement::new(Weigh, why, same_art, width, height)
}

/// True when a found cover of `found_side` px is enough larger to replace one of `own_side` px.
pub fn is_much_larger(found_side: u32, own_side: u32) -> bool {
    f64::from(found_side) >= f64::from(own_side) * MUCH_LARGER
}

/// Whether `own` holds much less fine detail than the reference at the size they share. Unknown
/// counts as no.
pub(crate) fn blown_up(own: &[u8], own_side: u32, reference: &[u8]) -> bool {
    let Some((reference_width, reference_height)) = cover_image::measure(reference) else {
        return false;
    };
    let side = DETAIL_SIDE.min(own_side.min(reference_width.min(reference_height)));
    let Some(theirs) = cover_image::detail(reference, side).filter(|theirs| *theirs >= MIN_REFERENCE_DETAIL)
    else {
        return false;
    };
    cover_image::detail(own, side).is_some_and(|mine| mine < theirs * BLOWN_UP_RATIO)
}

#[cfg(test)]
#[path = "cover_keep_tests.rs"]
pub(crate) mod tests;
