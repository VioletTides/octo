use chrono::TimeZone;

use super::*;

#[test]
fn the_next_period_starts_on_a_six_hour_boundary() {
    let at = |h: u32, m: u32| Utc.with_ymd_and_hms(2026, 10, 7, h, m, 0).unwrap();
    assert_eq!(
        until_next_period(at(9, 0)),
        Duration::from_secs(3 * 60 * 60) + AFTER_PERIOD
    );
    assert_eq!(
        until_next_period(at(17, 59)),
        Duration::from_secs(60) + AFTER_PERIOD
    );
    assert_eq!(
        until_next_period(at(18, 0)),
        Duration::from_secs(6 * 60 * 60) + AFTER_PERIOD
    );
}
