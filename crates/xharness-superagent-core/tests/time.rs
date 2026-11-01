use xharness_superagent_core::engine::time::{from_milliseconds, iso_time, milliseconds};
#[test]
fn iso_calendar_overflow_and_midnight_follow_ecmascript() {
    for (input, expected) in [
        ("2026", "2026-01-01T00:00:00.000Z"),
        ("2026-10", "2026-10-01T00:00:00.000Z"),
        ("2026-02-30", "2026-03-02T00:00:00.000Z"),
        ("1900-02-29", "1900-03-01T00:00:00.000Z"),
        ("2000-02-29", "2000-02-29T00:00:00.000Z"),
        ("2024-01-01T24:00Z", "2024-01-02T00:00:00.000Z"),
        ("2024-01-01t01:02:03.4567+0800", "2023-12-31T17:02:03.456Z"),
        ("Thu, 01 Jan 1970 00:00:00 GMT", "1970-01-01T00:00:00.000Z"),
        ("Jan 1 2024 3:04:05.6 pm GMT-8", "2024-01-01T23:04:05.600Z"),
        ("-000001-01-01", "-000001-01-01T00:00:00.000Z"),
    ] {
        assert_eq!(iso_time(input), expected, "{input}");
    }
}
#[test]
fn full_timeclip_range_formats_six_digit_signed_years() {
    for (ms, expected) in [
        (-8_640_000_000_000_000, "-271821-04-20T00:00:00.000Z"),
        (-1, "1969-12-31T23:59:59.999Z"),
        (0, "1970-01-01T00:00:00.000Z"),
        (253_402_300_800_000, "+010000-01-01T00:00:00.000Z"),
        (8_640_000_000_000_000, "+275760-09-13T00:00:00.000Z"),
    ] {
        assert_eq!(from_milliseconds(ms).unwrap(), expected);
        assert_eq!(milliseconds(expected), Some(ms));
    }
    assert!(from_milliseconds(i64::MIN).is_err());
    assert!(from_milliseconds(i64::MAX).is_err());
    assert!(from_milliseconds(8_640_000_000_000_001).is_err());
}
#[test]
fn rejected_dates_remain_original_instead_of_being_coerced() {
    for input in [
        "",
        "invalid",
        "2026-13-01",
        "2026-01-32",
        "2026-01-01T24:00:01Z",
        "2026-01-01T23:59:60Z",
        "2026-01-01T00:00:00+24:00",
        "+275760-09-13T00:00:00.001Z",
        "-271821-04-19T23:59:59.999Z",
    ] {
        assert_eq!(milliseconds(input), None, "{input}");
        assert_eq!(iso_time(input), input);
    }
}
