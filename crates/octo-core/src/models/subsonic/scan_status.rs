use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

/// Subsonic library scan status
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ScanStatus {
    pub scanning: bool,
    pub count: Option<i32>,
    /// When the last scan finished, as Navidrome wrote it (RFC 3339). While a scan runs it is
    /// still the previous scan's.
    pub last_scan: Option<String>,
    /// Navidrome's elapsedTime, in nanoseconds: how long the last scan took, or, while one
    /// runs, how long it has been going.
    pub elapsed_time: Option<i64>,
    pub folder_count: Option<i64>,
}

impl ScanStatus {
    /// When the last finished scan finished.
    pub fn finished_at(&self) -> Option<DateTime<Utc>> {
        let text = self.last_scan.as_deref()?;
        DateTime::parse_from_rfc3339(text.trim())
            .ok()
            .map(|time| time.with_timezone(&Utc))
    }

    /// When the last finished scan started: its finish less its length. None while a scan runs,
    /// since the times then describe two different scans.
    pub fn last_scan_started_at(&self) -> Option<DateTime<Utc>> {
        if self.scanning {
            return None;
        }
        let finished = self.finished_at()?;
        let elapsed = TimeDelta::nanoseconds(self.elapsed_time.unwrap_or(0).max(0));
        Some(finished - elapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finished_scan_started_its_length_before_it_finished() {
        let status = ScanStatus {
            scanning: false,
            last_scan: Some("2026-10-09T10:15:30.5+02:00".to_string()),
            elapsed_time: Some(90_500_000_000),
            ..ScanStatus::default()
        };
        let finished = status.finished_at().expect("a time");
        assert_eq!(finished.to_rfc3339(), "2026-10-09T08:15:30.500+00:00");
        assert_eq!(
            status.last_scan_started_at().expect("a time").to_rfc3339(),
            "2026-10-09T08:14:00+00:00"
        );
    }

    #[test]
    fn a_running_scan_has_no_start_to_measure_against() {
        let status = ScanStatus {
            scanning: true,
            last_scan: Some("2026-10-09T10:15:30Z".to_string()),
            elapsed_time: Some(1),
            ..ScanStatus::default()
        };
        assert!(status.last_scan_started_at().is_none());
        assert!(ScanStatus::default().finished_at().is_none());
    }
}
