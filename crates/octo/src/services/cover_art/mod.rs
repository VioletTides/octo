//! Cover-art lookups (`Services/CoverArt`): the sources behind the aggregator, the iTunes
//! master lookup, the Cover Art Archive, the chain a download's cover is chosen by, and the cover
//! upgrade with the album cover finder it asks. List-cover rendering is `octo_media::cover`.

pub mod album_cover_finder;
pub mod artwork_hints;
pub mod cover_art_aggregator;
pub mod cover_art_archive_lookup;
pub mod cover_keep;
pub mod cover_upgrade;
pub mod deezer_cover_art_lookup;
pub mod download_cover_resolver;
pub mod i_cover_art_source;
pub mod itunes_cover_art_lookup;
pub mod last_fm_cover_art_lookup;
pub mod local_cover_cache;

pub use album_cover_finder::{
    AlbumCoverFinder, AlbumCoverQuery, FoundCover, IAlbumCoverFinder, RejectedCovers,
};
pub use artwork_hints::{ArtworkHints, HintedArtwork};
pub use cover_art_aggregator::CoverArtAggregator;
pub use cover_art_archive_lookup::CoverArtArchiveLookup;
pub use cover_upgrade::{
    AlbumCoverReplaced, AlbumOnDisk, CoverUpgradeChange, CoverUpgradeItem, CoverUpgradeJournal,
    CoverUpgradeJournalEntry, CoverUpgradeMode, CoverUpgradeRequest, CoverUpgradeRun, CoverUpgradeScope,
    CoverUpgradeServices, CoverUpgradeStatus, CoverUpgradeStore, CoverUpgradeWorker,
};
pub use deezer_cover_art_lookup::DeezerCoverArtLookup;
pub use download_cover_resolver::{CoverChoice, CoversOnHand, DownloadCoverResolver};
pub use i_cover_art_source::ICoverArtSource;
pub use itunes_cover_art_lookup::ITunesCoverArtLookup;
pub use last_fm_cover_art_lookup::LastFmCoverArtLookup;
pub use local_cover_cache::LocalCoverCache;
