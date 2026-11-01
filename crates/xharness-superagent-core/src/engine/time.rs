use super::{DomainError, DomainResult};
use chrono::{
    DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone,
    Utc,
};
mod parser;
const DAY: i64 = 86_400_000;
const LIMIT: i64 = 8_640_000_000_000_000;
fn leap(y: i64) -> bool {
    y.rem_euclid(4) == 0 && (y.rem_euclid(100) != 0 || y.rem_euclid(400) == 0)
}
fn before_year(y: i64) -> i64 {
    365 * y + (y - 1).div_euclid(4) - (y - 1).div_euclid(100) + (y - 1).div_euclid(400)
}
fn days(y: i64, m: i64, d: i64) -> i64 {
    const MONTH: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    before_year(y) - before_year(1970) + MONTH[(m - 1) as usize] + i64::from(m > 2 && leap(y)) + d
        - 1
}
fn local_offset(base: i64) -> Option<i64> {
    let naive = DateTime::<Utc>::from_timestamp_millis(base)?.naive_utc();
    // Node's ICU carries its own tzdb. Prefer the locked Rust tzdb for IANA
    // names instead of silently inheriting a stale host zoneinfo database.
    let name = std::env::var("TZ")
        .ok()
        .or_else(|| iana_time_zone::get_timezone().ok());
    if let Some(name) = name {
        let name = name
            .trim_start_matches(':')
            .trim_start_matches("/usr/share/zoneinfo/");
        if let Ok(zone) = name.parse::<chrono_tz::Tz>() {
            return resolve_local(&zone, future_shape(zone, naive)?);
        }
    }
    resolve_local(&Local, naive)
}
// chrono-tz 0.10.4 materializes recurring transitions only through 2099;
// its final span is not proof that a DST zone becomes permanently fixed. For
// recurring future zones retain the Gregorian calendar shape inside that table.
// Zones whose final rules are fixed (including Morocco after its last published
// Ramadan exception and Apia after DST abolition) keep their final span.
fn future_shape(zone: chrono_tz::Tz, naive: NaiveDateTime) -> Option<NaiveDateTime> {
    if naive.year() < 2100 {
        return Some(naive);
    }
    let offsets: Vec<_> = [1, 4, 7, 10]
        .into_iter()
        .map(|m| {
            let date = NaiveDate::from_ymd_opt(2090, m, 1)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap();
            zone.from_utc_datetime(&date)
                .offset()
                .fix()
                .local_minus_utc()
        })
        .collect();
    if offsets.iter().all(|offset| *offset == offsets[0]) {
        return Some(naive);
    }
    let y = i64::from(naive.year());
    let target = days(y, 1, 1).rem_euclid(7);
    let equivalent =
        (2070..=2097).find(|&e| leap(e) == leap(y) && days(e, 1, 1).rem_euclid(7) == target)?;
    naive.with_year(equivalent as i32)
}
fn resolve_local<T: TimeZone>(zone: &T, naive: NaiveDateTime) -> Option<i64> {
    match zone.from_local_datetime(&naive) {
        LocalResult::Single(t) => Some(i64::from(t.offset().fix().local_minus_utc()) * 1000),
        LocalResult::Ambiguous(a, b) => Some(
            i64::from(if a.timestamp_millis() < b.timestamp_millis() {
                a.offset().fix().local_minus_utc()
            } else {
                b.offset().fix().local_minus_utc()
            }) * 1000,
        ),
        // JS resolves nonexistent wall time with the pre-transition offset;
        // account for both ordinary DST gaps and whole-day date-line changes.
        LocalResult::None => {
            (1..=48).find_map(
                |h| match zone.from_local_datetime(&(naive - Duration::hours(h))) {
                    LocalResult::Single(t) => {
                        Some(i64::from(t.offset().fix().local_minus_utc()) * 1000)
                    }
                    LocalResult::Ambiguous(a, _) => {
                        Some(i64::from(a.offset().fix().local_minus_utc()) * 1000)
                    }
                    LocalResult::None => None,
                },
            )
        }
    }
}
pub fn milliseconds(value: &str) -> Option<i64> {
    let ((y, m, d), time, offset) = parser::parse(value)?;
    let base = days(i64::from(y), i64::from(m), i64::from(d))
        .checked_mul(DAY)?
        .checked_add(time)?;
    let offset = match offset {
        Some(offset) => offset,
        None => {
            if let Some(offset) = local_offset(base) {
                offset
            } else {
                // Preserve the 400-year Gregorian shape outside chrono's civil
                // range. Past dates must use the zone's earliest (often LMT)
                // rule, not a modern DST offset; future dates use future rules.
                let equivalent = i64::from(y).rem_euclid(400) + if y < 0 { -10400 } else { 2400 };
                local_offset(days(equivalent, i64::from(m), i64::from(d)) * DAY + time)?
            }
        }
    };
    let result = base.checked_sub(offset)?;
    (-LIMIT..=LIMIT).contains(&result).then_some(result)
}
pub fn from_milliseconds(value: i64) -> DomainResult<String> {
    if !(-LIMIT..=LIMIT).contains(&value) {
        return Err(DomainError::Internal("Invalid time value".into()));
    }
    let day = value.div_euclid(DAY);
    let within = value.rem_euclid(DAY);
    let mut y = 1970 + day.div_euclid(365);
    while days(y, 1, 1) > day {
        y -= 1
    }
    while days(y + 1, 1, 1) <= day {
        y += 1
    }
    let mut m = 1;
    while m < 12 && days(y, m + 1, 1) <= day {
        m += 1
    }
    let d = day - days(y, m, 1) + 1;
    let year = if (0..=9999).contains(&y) {
        format!("{y:04}")
    } else {
        format!("{}{abs:06}", if y < 0 { "-" } else { "+" }, abs = y.abs())
    };
    Ok(format!(
        "{year}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        within / 3_600_000,
        within / 60_000 % 60,
        within / 1000 % 60,
        within % 1000
    ))
}
pub fn iso_time(value: &str) -> String {
    milliseconds(value)
        .and_then(|value| from_milliseconds(value).ok())
        .unwrap_or_else(|| value.into())
}
pub(super) fn add(value: &str, duration: i64) -> DomainResult<String> {
    let base =
        milliseconds(value).ok_or_else(|| DomainError::Internal("Invalid time value".into()))?;
    from_milliseconds(
        base.checked_add(duration)
            .ok_or_else(|| DomainError::Internal("Invalid time value".into()))?,
    )
}
pub(super) fn later(left: &str, right: &str) -> bool {
    milliseconds(left)
        .zip(milliseconds(right))
        .is_some_and(|(left, right)| left > right)
}
