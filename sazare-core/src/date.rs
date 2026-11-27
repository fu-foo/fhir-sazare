//! FHIR date / dateTime values as time ranges.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

/// Parse a FHIR date/dateTime string into a half-open epoch-**microsecond**
/// range `[start, end)` that reflects the value's precision: a year spans a
/// year, a date a day, a whole-second dateTime one second, and a dateTime with
/// fractional seconds one microsecond (effectively exact, so e.g. `_lastUpdated`
/// instants that differ only in sub-second digits do not collide). Returns
/// `None` if the value cannot be parsed.
pub fn fhir_date_range(value: &str) -> Option<(i64, i64)> {
    let v = value.trim();
    if v.contains('T') {
        // dateTime — require a timezone per FHIR; try RFC3339, then assume UTC.
        // Minute precision (`YYYY-MM-DDThh:mm`, no seconds) is a valid FHIR
        // dateTime and spans one minute.
        if let Ok(dt) = DateTime::parse_from_rfc3339(v) {
            let start = dt.with_timezone(&Utc).timestamp_micros();
            let span = if v.contains('.') { 1 } else { 1_000_000 };
            return Some((start, start + span));
        }
        if let Ok(n) = chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S") {
            let start = Utc.from_utc_datetime(&n).timestamp_micros();
            return Some((start, start + 1_000_000));
        }
        // Minute precision, with or without a trailing `Z` (offset forms at
        // minute precision are uncommon and left unparsed).
        let naive = v.trim_end_matches('Z');
        if let Ok(n) = chrono::NaiveDateTime::parse_from_str(naive, "%Y-%m-%dT%H:%M") {
            let start = Utc.from_utc_datetime(&n).timestamp_micros();
            return Some((start, start + 60_000_000));
        }
        return None;
    }
    // A date is ASCII digits with `-` between the parts: anything else isn't
    // one (and slicing by bytes below is then safe).
    let shape_ok = v.is_ascii()
        && v.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() });
    if !shape_ok {
        return None;
    }
    match v.len() {
        4 => {
            let year: i32 = v.parse().ok()?;
            let start = Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).single()?;
            let end = Utc.with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0).single()?;
            Some((start.timestamp_micros(), end.timestamp_micros()))
        }
        7 => {
            let year: i32 = v[0..4].parse().ok()?;
            let month: u32 = v[5..7].parse().ok()?;
            let start = Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0).single()?;
            let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
            let end = Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0).single()?;
            Some((start.timestamp_micros(), end.timestamp_micros()))
        }
        10 => {
            let d = NaiveDate::parse_from_str(v, "%Y-%m-%d").ok()?;
            let start = Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?);
            let end = start + chrono::Duration::days(1);
            Some((start.timestamp_micros(), end.timestamp_micros()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::fhir_date_range;

    #[test]
    fn dates_and_not_dates() {
        for ok in ["2024", "2024-06", "2024-06-15", "2024-06-15T10:00:00Z", "2024-06-15T10:00:00+09:00", "2024-06-15T10:00"] {
            assert!(fhir_date_range(ok).is_some(), "{ok}");
        }
        // Not dates, and none panics (2024é1 is 7 bytes: slicing it by bytes would).
        for bad in ["2024X06", "2024é1", "2024年1", "20x4", "2024-6", "2024-06-1", "2024-13", "2024-02-30", "", "abcd-ef"] {
            assert!(fhir_date_range(bad).is_none(), "{bad}");
        }
    }
}
