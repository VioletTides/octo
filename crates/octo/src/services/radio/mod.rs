//! Radio's suggestion sources (Last.fm, YouTube Music, ListenBrainz and Sounds alike), the set
//! that asks them together, song radio (getSimilarSongs) and what radio learns from listening
//! (`radio-outcomes.json`). The records and the blend are `octo_core::radio`.

pub mod last_fm_radio_source;
pub mod listen_brainz_radio_source;
pub mod radio_outcome_store;
pub mod radio_source;
pub mod radio_source_set;
pub mod song_radio_service;
pub mod sounds_alike_radio_source;
pub mod you_tube_music_radio_source;

pub use last_fm_radio_source::LastFmRadioSource;
pub use listen_brainz_radio_source::ListenBrainzRadioSource;
pub use radio_outcome_store::{OutcomeCounts, OutcomeState, ProviderOutcome, RadioOutcomeStore};
pub use radio_source::IRadioSource;
pub use radio_source_set::RadioSourceSet;
pub use song_radio_service::SongRadioService;
pub use sounds_alike_radio_source::SoundsAlikeRadioSource;
pub use you_tube_music_radio_source::YouTubeMusicRadioSource;
