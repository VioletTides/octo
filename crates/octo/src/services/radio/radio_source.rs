//! `IRadioSource`: a place radio's suggestions come from. The records it answers with are
//! `octo_core::radio::radio_types`.

use async_trait::async_trait;
use indexmap::IndexMap;
use octo_core::radio::{RadioAnswer, RadioSeed};

/// A place radio's suggestions come from.
#[async_trait]
pub trait IRadioSource: Send + Sync {
    /// A `radio_provider` key.
    fn provider(&self) -> &'static str;

    /// Switched on and set up. A source that is unreachable still says true and answers nothing.
    fn available(&self) -> bool;

    /// Songs like the seed. `Err` when the source fails: the set counts it as a failure.
    async fn songs_like(
        &self,
        seed: &RadioSeed,
        count: i32,
        auth: &IndexMap<String, String>,
    ) -> anyhow::Result<RadioAnswer>;

    /// Songs for a station built on a tag ("phonk"). Most sources have none.
    async fn tag(&self, _tag: &str, _count: i32) -> anyhow::Result<RadioAnswer> {
        Ok(RadioAnswer::nothing(self.provider()))
    }
}
