//! Sounds alike, octo-sonic's reading of every library song: the client, the analysis pass, and
//! what the analysis keeps on disk (`sonic-features.json`) with the nearest-song search, and each
//! song's transition profile (`transition-profiles.json`).

pub mod sonic_analysis_worker;
pub mod sonic_client;
pub mod sonic_store;
pub mod transition_store;

pub use sonic_analysis_worker::{
    IPublishedTempo, ISonicLibrary, NavidromeSonicLibrary, SonicAnalysisWorker, SonicFile, SonicStatus,
    file_tempo_tags,
};
pub use sonic_client::{
    ISonicClient, SonicAnswer, SonicClient, SonicDropped, SonicFeatures, SonicHealth, SonicParts,
};
pub use sonic_store::{SonicFailure, SonicSong, SonicState, SonicStore};
pub use transition_store::{TransitionEntry, TransitionLookup, TransitionStore};
