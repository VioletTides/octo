//! Dates in Octo's JSON, written in one fixed shape and read in any of its forms.
//!
//! Dates are written in ISO 8601 with up to seven fractional digits (100 ns ticks), trailing
//! zeros trimmed: a UTC instant ends in `Z`, a date and time without a zone (including
//! [`min_value`], which stands for an unset date) has no suffix, and an instant with an offset
//! always carries it (`+00:00`). Reading accepts any of these forms.
//!
//! Use with `#[serde(with = "octo_core::json::datetime::utc")]` and friends.

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, TimeZone, Utc};

/// The earliest date, 0001-01-01T00:00:00, which stands for an unset date and is always written
/// without a `Z`.
pub fn min_value() -> DateTime<Utc> {
    NaiveDate::from_ymd_opt(1, 1, 1)
        .unwrap_or(NaiveDate::MIN)
        .and_time(NaiveTime::MIN)
        .and_utc()
}

/// The fractional-seconds part: ticks (100 ns), trailing zeros trimmed, nothing at all when zero.
fn fraction(nanos: u32) -> String {
    let ticks = nanos / 100;
    if ticks == 0 {
        return String::new();
    }
    let digits = format!("{ticks:07}");
    format!(".{}", digits.trim_end_matches('0'))
}

/// A UTC instant: `2026-10-04T12:34:56.12345Z`. [`min_value`] is written without the `Z`, as
/// an unset date.
pub fn format_utc(value: &DateTime<Utc>) -> String {
    let base = value.format("%Y-%m-%dT%H:%M:%S").to_string();
    let frac = fraction(value.timestamp_subsec_nanos());
    if *value == min_value() {
        format!("{base}{frac}")
    } else {
        format!("{base}{frac}Z")
    }
}

/// An instant with its offset: `2026-10-04T12:34:56+00:00`.
pub fn format_offset(value: &DateTime<FixedOffset>) -> String {
    let base = value.format("%Y-%m-%dT%H:%M:%S").to_string();
    let frac = fraction(value.timestamp_subsec_nanos());
    let off = value.format("%:z").to_string();
    format!("{base}{frac}{off}")
}

/// A UTC instant written with offset zero (`+00:00`).
pub fn format_utc_as_offset(value: &DateTime<Utc>) -> String {
    format_offset(&value.fixed_offset())
}

/// A local date and time without a zone.
pub fn format_naive(value: &NaiveDateTime) -> String {
    let base = value.format("%Y-%m-%dT%H:%M:%S").to_string();
    format!("{base}{}", fraction(value.and_utc().timestamp_subsec_nanos()))
}

/// Parses an ISO 8601 date. A value without an offset is taken as UTC, which is what Octo's
/// writers meant by it.
pub fn parse_utc(text: &str) -> Option<DateTime<Utc>> {
    let t = text.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
        return Some(dt.with_timezone(&Utc));
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S%.f",
    ] {
        if let Ok(n) = NaiveDateTime::parse_from_str(t, fmt) {
            return Some(Utc.from_utc_datetime(&n));
        }
    }
    NaiveDate::parse_from_str(t, "%Y-%m-%d")
        .ok()
        .map(|d| d.and_time(NaiveTime::MIN).and_utc())
}

/// Parses an instant with its offset; a value without an offset is taken as UTC.
pub fn parse_offset(text: &str) -> Option<DateTime<FixedOffset>> {
    let t = text.trim();
    DateTime::parse_from_rfc3339(t)
        .ok()
        .or_else(|| parse_utc(t).map(|u| u.fixed_offset()))
}

/// ISO 8601 with milliseconds and `Z`, as JavaScript's `toISOString` writes, for the places
/// Octo formats a date itself.
pub fn to_iso_millis(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// `#[serde(with = "utc")]` for a UTC instant.
pub mod utc {
    use super::*;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format_utc(value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        let text = String::deserialize(d)?;
        parse_utc(&text).ok_or_else(|| D::Error::custom(format!("not a date: {text}")))
    }
}

/// `#[serde(with = "utc_option")]` for an optional UTC instant.
pub mod utc_option {
    use super::*;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(v) => s.serialize_str(&format_utc(v)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<DateTime<Utc>>, D::Error> {
        match Option::<String>::deserialize(d)? {
            Some(text) => parse_utc(&text)
                .map(Some)
                .ok_or_else(|| D::Error::custom(format!("not a date: {text}"))),
            None => Ok(None),
        }
    }
}

/// `#[serde(with = "offset")]` for an instant with its offset.
pub mod offset {
    use super::*;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &DateTime<FixedOffset>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format_offset(value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<FixedOffset>, D::Error> {
        let text = String::deserialize(d)?;
        parse_offset(&text).ok_or_else(|| D::Error::custom(format!("not a date: {text}")))
    }
}

/// `#[serde(with = "offset_option")]` for an optional instant with its offset.
pub mod offset_option {
    use super::*;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &Option<DateTime<FixedOffset>>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(v) => s.serialize_str(&format_offset(v)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<DateTime<FixedOffset>>, D::Error> {
        match Option::<String>::deserialize(d)? {
            Some(text) => parse_offset(&text)
                .map(Some)
                .ok_or_else(|| D::Error::custom(format!("not a date: {text}"))),
            None => Ok(None),
        }
    }
}

/// `#[serde(with = "utc_offset")]` for an instant written with its offset that Octo only ever
/// sets to UTC: kept as `DateTime<Utc>`, written with `+00:00`.
pub mod utc_offset {
    use super::*;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format_utc_as_offset(value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        let text = String::deserialize(d)?;
        parse_utc(&text).ok_or_else(|| D::Error::custom(format!("not a date: {text}")))
    }
}

/// `#[serde(with = "utc_offset_option")]` for an optional UTC instant written with its offset.
pub mod utc_offset_option {
    use super::*;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(v) => s.serialize_str(&format_utc_as_offset(v)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<DateTime<Utc>>, D::Error> {
        match Option::<String>::deserialize(d)? {
            Some(text) => parse_utc(&text)
                .map(Some)
                .ok_or_else(|| D::Error::custom(format!("not a date: {text}"))),
            None => Ok(None),
        }
    }
}

/// A duration as `[-][d.]hh:mm:ss[.fffffff]`, always seven fractional digits when there is a
/// fraction (`00:01:30.5000000`).
pub fn format_timespan(value: &chrono::TimeDelta) -> String {
    let negative = *value < chrono::TimeDelta::zero();
    let total = value.abs();
    let ticks = total.num_microseconds().map(|us| us * 10).unwrap_or(i64::MAX);
    let secs = ticks / 10_000_000;
    let frac = ticks % 10_000_000;
    let days = secs / 86_400;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if days > 0 {
        out.push_str(&format!("{days}."));
    }
    out.push_str(&format!("{h:02}:{m:02}:{s:02}"));
    if frac > 0 {
        out.push_str(&format!(".{frac:07}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates_trim_trailing_fraction_zeros_and_end_in_z() {
        let dt = Utc.with_ymd_and_hms(2026, 10, 4, 12, 34, 56).unwrap();
        assert_eq!(format_utc(&dt), "2026-10-04T12:34:56Z");
        let dt = dt + chrono::TimeDelta::nanoseconds(123_450_000);
        assert_eq!(format_utc(&dt), "2026-10-04T12:34:56.12345Z");
    }

    #[test]
    fn min_value_has_no_zone_suffix() {
        assert_eq!(format_utc(&min_value()), "0001-01-01T00:00:00");
        assert_eq!(parse_utc("0001-01-01T00:00:00"), Some(min_value()));
    }

    #[test]
    fn offsets_are_written_in_full() {
        let dt = FixedOffset::east_opt(7 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 4, 12, 34, 56)
            .unwrap()
            + chrono::TimeDelta::nanoseconds(1000);
        assert_eq!(format_offset(&dt), "2026-10-04T12:34:56.000001+07:00");
        let utc = Utc.with_ymd_and_hms(2026, 10, 4, 12, 34, 56).unwrap();
        assert_eq!(format_utc_as_offset(&utc), "2026-10-04T12:34:56+00:00");
    }

    #[test]
    fn timespans_write_days_and_seven_fraction_digits() {
        assert_eq!(
            format_timespan(&chrono::TimeDelta::milliseconds(90_500)),
            "00:01:30.5000000"
        );
        assert_eq!(format_timespan(&chrono::TimeDelta::hours(26)), "1.02:00:00");
    }
}
