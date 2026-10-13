//! Multi-source radio, the pure parts (`Services/Radio`): where a suggestion came from
//! (`RadioProvider`), what a radio starts from and what a source answers (`RadioTypes`), and how
//! the answers are blended (`RadioBlend`). The sources, the set that asks them, song radio and
//! what radio learns from listening are `octo::services::radio`.

pub mod radio_blend;
pub mod radio_provider;
pub mod radio_types;

pub use radio_blend::RadioPick;
pub use radio_types::{RadioAnswer, RadioMatch, RadioSeed};
