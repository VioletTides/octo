//! Navidrome keeps a song whose file went away as a missing row, hidden from listings, with its
//! plays, favorite, rating and bookmark still on it, until someone removes it. A row stays as
//! long as its file could come back. Once the trash sweep deletes a file for good, its row can
//! never be found again, so it is removed from Navidrome the way Navidrome's own Missing Files
//! page removes one. A row that still carries the signed-in user's plays, favorite, rating or
//! bookmark is left for a person to decide.

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;
use tracing::{info, warn};

use crate::services::family::NavidromeAdmin;

/// How many ids go in one removal call, keeping the address short.
const IDS_PER_CALL: usize = 50;

/// What one pass did with the ids it was given and the ones still waiting.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Forgotten {
    /// Rows removed from Navidrome.
    pub removed: Vec<String>,
    /// Rows left in Navidrome because they still carry plays, a favorite, a rating or a
    /// bookmark.
    pub kept: Vec<String>,
    /// Ids Navidrome could not be asked about or could not remove; tried again next pass.
    pub waiting: Vec<String>,
}

/// Whether a missing row still carries the signed-in user's history.
pub fn carries_history(song: &Value) -> bool {
    let number = |name: &str| {
        song.get(name)
            .and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok()))
            .unwrap_or(0.0)
    };
    let starred = song.get("starred").and_then(Value::as_bool).unwrap_or(false);
    starred || number("playCount") > 0.0 || number("rating") > 0.0 || number("bookmarkPosition") > 0.0
}

/// Removes the Navidrome rows of songs deleted for good.
pub struct MissingRows {
    admin: Arc<NavidromeAdmin>,
    waiting: Mutex<Vec<String>>,
}

impl MissingRows {
    pub fn new(admin: Arc<NavidromeAdmin>) -> Self {
        MissingRows {
            admin,
            waiting: Mutex::new(Vec::new()),
        }
    }

    /// The ids still waiting for Navidrome.
    pub fn waiting(&self) -> Vec<String> {
        self.waiting.lock().clone()
    }

    /// Asks Navidrome about `ids` and every id still waiting from earlier passes, and removes
    /// each one it still keeps as missing with no history on it. Nothing is sent when there is
    /// nothing to ask about.
    pub async fn forget(&self, ids: Vec<String>) -> Forgotten {
        let asking: Vec<String> = {
            let mut waiting = self.waiting.lock();
            for id in ids {
                let id = id.trim().to_string();
                if !id.is_empty() && !waiting.contains(&id) {
                    waiting.push(id);
                }
            }
            std::mem::take(&mut *waiting)
        };
        let mut done = Forgotten::default();
        if asking.is_empty() {
            return done;
        }

        let mut removable = Vec::new();
        for id in asking {
            match self.admin.missing_song(&id).await {
                Ok(None) => {}
                Ok(Some(song)) if carries_history(&song) => done.kept.push(id),
                Ok(Some(_)) => removable.push(id),
                Err(e) => {
                    warn!("Could not ask Navidrome about missing song {id}: {e}");
                    done.waiting.push(id);
                }
            }
        }

        for ids in removable.chunks(IDS_PER_CALL) {
            match self.admin.delete_missing(ids).await {
                Ok(()) => done.removed.extend_from_slice(ids),
                Err(e) => {
                    warn!("Navidrome did not remove {} missing song(s): {e}", ids.len());
                    done.waiting.extend_from_slice(ids);
                }
            }
        }

        if !done.removed.is_empty() {
            info!(
                "Removed {} song(s) deleted for good from Navidrome's missing files",
                done.removed.len()
            );
        }
        if !done.kept.is_empty() {
            info!(
                "Left {} missing song(s) in Navidrome because they still have plays, a favorite, a rating or a bookmark: {}",
                done.kept.len(),
                done.kept.join(", ")
            );
        }
        self.waiting.lock().extend(done.waiting.iter().cloned());
        done
    }
}

#[cfg(test)]
#[path = "missing_rows_tests.rs"]
mod tests;
