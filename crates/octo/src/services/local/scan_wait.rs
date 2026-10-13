//! Holding Octo's own scan triggers back, and waiting for a Navidrome scan that saw a change.
//!
//! The library tidy changes a song's tags, waits for a scan that started after the change, and
//! only then moves the file. A scan Octo itself starts in between (after a download, after a
//! lyrics write) could start while files are moving, so while the tidy holds the scans those
//! triggers are remembered rather than sent, and one scan is sent when the hold ends.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use super::ILocalLibraryService;

/// Whether Octo's own scan triggers are held, and whether one came while they were.
#[derive(Clone, Default)]
pub struct ScanHold {
    inner: Arc<HoldState>,
}

#[derive(Default)]
struct HoldState {
    holders: AtomicUsize,
    wanted: AtomicBool,
}

impl ScanHold {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold every trigger until the guard is released or dropped.
    pub fn hold(&self) -> ScanHoldGuard {
        self.inner.holders.fetch_add(1, Ordering::SeqCst);
        ScanHoldGuard {
            hold: self.clone(),
            released: false,
        }
    }

    pub fn is_held(&self) -> bool {
        self.inner.holders.load(Ordering::SeqCst) > 0
    }

    /// A trigger that came while held. True when it was held back.
    pub fn defer(&self) -> bool {
        if !self.is_held() {
            return false;
        }
        self.inner.wanted.store(true, Ordering::SeqCst);
        true
    }
}

/// One hold. Releasing it says whether a scan was asked for meanwhile; the last holder to let
/// go is the one told.
pub struct ScanHoldGuard {
    hold: ScanHold,
    released: bool,
}

impl ScanHoldGuard {
    /// Lets go. True when this was the last hold and a scan was asked for while it lasted, so
    /// the caller sends one.
    pub fn release(mut self) -> bool {
        self.released = true;
        self.let_go()
    }

    fn let_go(&self) -> bool {
        let before = self.hold.inner.holders.fetch_sub(1, Ordering::SeqCst);
        before == 1 && self.hold.inner.wanted.swap(false, Ordering::SeqCst)
    }
}

impl Drop for ScanHoldGuard {
    fn drop(&mut self) {
        if !self.released {
            // The wanted mark stays set when nobody is told, so the next trigger still goes.
            let before = self.hold.inner.holders.fetch_sub(1, Ordering::SeqCst);
            debug_assert!(before > 0);
        }
    }
}

/// Unanswered status requests in a row before the wait gives up.
const SILENT_LIMIT: u32 = 3;

/// Why no scan was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanWaitError {
    /// Navidrome did not answer a scan status request.
    NoStatus,
    /// No finished scan that started after the change came in time.
    TimedOut,
    /// The wait was cancelled.
    Cancelled,
}

/// How often to ask, and for how long at most.
#[derive(Debug, Clone, Copy)]
pub struct ScanWaitTiming {
    pub poll: Duration,
    pub timeout: Duration,
}

impl Default for ScanWaitTiming {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(5),
            timeout: Duration::from_secs(60 * 60),
        }
    }
}

/// Wait until Navidrome is idle and its last scan started after `after`. Answers that scan's
/// finish time. A scan that was already running when the change was made may have read the
/// file before it, so only a start after `after` counts.
pub async fn wait_for_scan(
    library: &dyn ILocalLibraryService,
    after: DateTime<Utc>,
    timing: ScanWaitTiming,
    cancel: &CancellationToken,
) -> Result<DateTime<Utc>, ScanWaitError> {
    let deadline = tokio::time::Instant::now() + timing.timeout;
    let mut silent = 0;
    loop {
        match library.get_scan_status().await {
            Some(status) => {
                silent = 0;
                if let (Some(started), Some(finished)) = (status.last_scan_started_at(), status.finished_at())
                    && started > after
                {
                    return Ok(finished);
                }
            }
            None => {
                silent += 1;
                if silent >= SILENT_LIMIT {
                    return Err(ScanWaitError::NoStatus);
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ScanWaitError::TimedOut);
        }
        tokio::select! {
            () = tokio::time::sleep(timing.poll) => {}
            () = cancel.cancelled() => return Err(ScanWaitError::Cancelled),
        }
    }
}

/// Wait while Navidrome reports a scan running. Answers false when it never stopped in time,
/// did not answer, or the wait was cancelled.
pub async fn wait_while_scanning(
    library: &dyn ILocalLibraryService,
    timing: ScanWaitTiming,
    cancel: &CancellationToken,
) -> bool {
    let deadline = tokio::time::Instant::now() + timing.timeout;
    loop {
        match library.get_scan_status().await {
            Some(status) if !status.scanning => return true,
            None => return false,
            Some(_) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::select! {
            () = tokio::time::sleep(timing.poll) => {}
            () = cancel.cancelled() => return false,
        }
    }
}

#[cfg(test)]
#[path = "scan_wait_tests.rs"]
mod tests;
