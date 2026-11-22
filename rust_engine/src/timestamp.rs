#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedTimestamp {
    Millis(i64),
    BeyondI64,
}

// Accept ASCII digits only.
fn number(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }

    let mut result = 0i64;

    for &byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }

        result = result.checked_mul(10)?.checked_add((byte - b'0') as i64)?;
    }

    Some(result)
}

fn leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: i64) -> Option<i64> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap_year(year) => 29,
        2 => 28,
        _ => return None,
    })
}

// Days since 1970-01-01.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;

    era * 146097 + doe - 719468
}

pub fn parse_timestamp(raw: &[u8]) -> Option<ParsedTimestamp> {
    use ParsedTimestamp::{BeyondI64, Millis};

    if raw.is_empty() {
        return None;
    }

    // F3: Unix epoch milliseconds, ASCII digits only.
    if raw.iter().all(|byte| byte.is_ascii_digit()) {
        let first_nonzero = raw
            .iter()
            .position(|&byte| byte != b'0')
            .unwrap_or(raw.len());

        if first_nonzero == raw.len() {
            return Some(Millis(0));
        }

        return Some(match number(&raw[first_nonzero..]) {
            Some(ms) => Millis(ms),
            None => BeyondI64,
        });
    }

    // F1: YYYY-MM-DDTHH:MM:SSZ
    let f1 = raw.len() == 20 && raw[19] == b'Z';

    // F2: YYYY-MM-DDTHH:MM:SS+HH:MM or -HH:MM
    let f2 = raw.len() == 25 && (raw[19] == b'+' || raw[19] == b'-') && raw[22] == b':';

    if !f1 && !f2 {
        return None;
    }

    if raw[4] != b'-' || raw[7] != b'-' || raw[10] != b'T' || raw[13] != b':' || raw[16] != b':' {
        return None;
    }

    let year = number(&raw[0..4])?;
    let month = number(&raw[5..7])?;
    let day = number(&raw[8..10])?;
    let hour = number(&raw[11..13])?;
    let minute = number(&raw[14..16])?;
    let second = number(&raw[17..19])?;

    if !(1..=9999).contains(&year)
        || day < 1
        || day > days_in_month(year, month)?
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    let days = days_from_civil(year, month, day);

    let local_seconds = days * 86400 + hour * 3600 + minute * 60 + second;

    let offset_seconds = if f2 {
        let offset_hours = number(&raw[20..22])?;
        let offset_minutes = number(&raw[23..25])?;

        if offset_hours > 23 || offset_minutes > 59 {
            return None;
        }

        let sign = if raw[19] == b'+' { 1 } else { -1 };

        sign * (offset_hours * 3600 + offset_minutes * 60)
    } else {
        0
    };

    Some(Millis((local_seconds - offset_seconds) * 1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_utc() {
        assert_eq!(
            parse_timestamp(b"2026-10-03T10:00:00Z"),
            Some(ParsedTimestamp::Millis(1791021600000))
        );
    }

    #[test]
    fn converts_timezone_offsets() {
        assert_eq!(
            parse_timestamp(b"2026-10-04T01:30:00+02:00"),
            parse_timestamp(b"2026-10-03T23:30:00Z")
        );

        assert_eq!(
            parse_timestamp(b"2026-10-04T09:59:00+23:59"),
            parse_timestamp(b"2026-10-03T10:00:00Z")
        );
    }

    #[test]
    fn parses_epoch_milliseconds() {
        assert_eq!(
            parse_timestamp(b"0001791021600000"),
            Some(ParsedTimestamp::Millis(1791021600000))
        );

        assert_eq!(
            parse_timestamp(b"179102160000000000000"),
            Some(ParsedTimestamp::BeyondI64)
        );
    }

    #[test]
    fn rejects_invalid_timestamps() {
        for raw in [
            "2026-10-03 10:00:00",
            "2026-10-03T10:00:00+24:00",
            "2026-10-03T10:00:00+05:60",
            "2026-10-03T10:00:00+0200",
            "2026-02-30T10:00:00Z",
            "2026-10-03T10:00:60Z",
            "2026-10-03T10:00:00Z\r",
            "١٧٩١٠٢١٦٠٠٠٠٠",
        ] {
            assert_eq!(parse_timestamp(raw.as_bytes()), None);
        }
    }

    #[test]
    fn preserves_millisecond_precision() {
        let start = parse_timestamp(b"1791021600000").unwrap();
        let exact = parse_timestamp(b"1791023400000").unwrap();
        let over = parse_timestamp(b"1791023400001").unwrap();

        let (
            ParsedTimestamp::Millis(start),
            ParsedTimestamp::Millis(exact),
            ParsedTimestamp::Millis(over),
        ) = (start, exact, over)
        else {
            panic!("Expected valid millisecond timestamps");
        };

        assert_eq!(exact - start, 1_800_000);
        assert_eq!(over - start, 1_800_001);
    }

    #[test]
    fn verifies_exact_millisecond_boundaries() {
        use ParsedTimestamp::Millis;

        let start = parse_timestamp(b"1791021600000");
        let exactly_30_minutes = parse_timestamp(b"1791023400000");
        let one_ms_over = parse_timestamp(b"1791023400001");

        assert_eq!(start, Some(Millis(1791021600000)));
        assert_eq!(exactly_30_minutes, Some(Millis(1791023400000)));
        assert_eq!(one_ms_over, Some(Millis(1791023400001)));

        // These three formats represent exactly the same UTC instant.
        assert_eq!(parse_timestamp(b"2026-10-03T10:00:00Z"), start);
        assert_eq!(parse_timestamp(b"2026-10-03T12:00:00+02:00"), start);
        assert_eq!(parse_timestamp(b"2026-10-03T05:30:00-04:30"), start);

        let Some(Millis(t0)) = start else {
            panic!("Expected a timestamp in integer milliseconds");
        };
        let Some(Millis(t30)) = exactly_30_minutes else {
            panic!("Expected a timestamp in integer milliseconds");
        };
        let Some(Millis(t30_plus_one)) = one_ms_over else {
            panic!("Expected a timestamp in integer milliseconds");
        };

        // Sessionization must use > 1,800,000 ms, not >=.
        assert_eq!(t30 - t0, 1_800_000);
        assert_eq!(t30_plus_one - t0, 1_800_001);
    }
}
