//! Audio tags, written and read with lofty, laid out the way TagLibSharp 2.3.0 writes them.
//!
//! [`TagFile`] holds a file's tags and their generic fields: each container's own tag is kept
//! in TagLib#'s model (ID3v2 frames, a Vorbis comment's fields, an iTunes item list's boxes)
//! and rendered the way TagLib# renders it, so a file tagged here reads back like one TagLib#
//! tagged. `fixture_tests` compares the files frame by frame against reference files.

mod apple;
mod genres;
mod id3;
mod net;
mod net_dictionary;
mod xiph;

pub mod audio_container;
pub mod kept_identity;
pub mod release_fact_tags;
pub mod song_tags;
pub mod tag_file;
pub mod tag_writer_extras;

#[cfg(test)]
mod dump;
#[cfg(test)]
mod fixture_tests;
#[cfg(test)]
mod kept_identity_tests;
#[cfg(test)]
mod tag_writer_extras_tests;
#[cfg(test)]
pub(crate) mod test_support;

pub use kept_identity::{KeptIdentity, KeptIdentityTags};
pub use song_tags::{
    GenreWrite, embed_cover, front_cover, read_lyrics, write_album_gain, write_lyrics, write_song,
};
pub use tag_file::{FRONT_COVER, Format, TagError, TagFile, TagPicture};
pub use tag_writer_extras::{TagField, TagFields, TagWriterExtras};
