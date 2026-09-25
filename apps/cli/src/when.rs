//! Times a person types for `--at`, and times shown back, in Windows local
//! time.

use fetchpath_protocol::Timestamp;
use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::Time::{
    SystemTimeToTzSpecificLocalTime, TzSpecificLocalTimeToSystemTime,
};

pub const FORMS: &str = "HH:MM (the next time the clock shows it), YYYY-MM-DD HH:MM, +30m, +2h, +1d, or an RFC 3339 UTC time";

/// Parses a start time relative to `now`.
pub fn parse(text: &str, now: Timestamp) -> Result<Timestamp, String> {
    let text = text.trim();
    let invalid = || format!("{text:?} is not a time Fetchpath understands. Use {FORMS}.");
    if let Some(amount) = text.strip_prefix('+') {
        let split = amount.len().saturating_sub(1);
        let (number, unit) = amount.split_at(split);
        let number: i64 = number.parse().map_err(|_| invalid())?;
        let seconds = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3_600,
            "d" => 86_400,
            _ => return Err(invalid()),
        };
        let ms = number
            .checked_mul(seconds * 1_000)
            .filter(|ms| (0..=366 * 86_400_000).contains(ms))
            .ok_or_else(invalid)?;
        return Ok(Timestamp::from_unix_ms(now.unix_ms() + ms));
    }
    if text.ends_with('Z') {
        return Timestamp::parse(text).map_err(|_| invalid());
    }
    if let Some((date, time)) = text.split_once([' ', 'T']) {
        let (year, month, day) = date_parts(date).ok_or_else(invalid)?;
        let (hour, minute) = time_parts(time).ok_or_else(invalid)?;
        return from_local(year, month, day, hour, minute).ok_or_else(invalid);
    }
    let (hour, minute) = time_parts(text).ok_or_else(invalid)?;
    let today = to_local(now).ok_or_else(invalid)?;
    let candidate =
        from_local(today.wYear, today.wMonth, today.wDay, hour, minute).ok_or_else(invalid)?;
    if candidate.unix_ms() > now.unix_ms() {
        return Ok(candidate);
    }
    let tomorrow =
        to_local(Timestamp::from_unix_ms(now.unix_ms() + 86_400_000)).ok_or_else(invalid)?;
    from_local(tomorrow.wYear, tomorrow.wMonth, tomorrow.wDay, hour, minute).ok_or_else(invalid)
}

/// A time as the person's clock shows it, such as `2026-09-24 18:30`.
pub fn local(at: Timestamp) -> String {
    match to_local(at) {
        Some(time) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute
        ),
        None => at.to_string(),
    }
}

fn date_parts(text: &str) -> Option<(u16, u16, u16)> {
    let mut parts = text.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((year, month, day))
}

fn time_parts(text: &str) -> Option<(u16, u16)> {
    let (hour, minute) = text.split_once(':')?;
    let hour: u16 = hour.parse().ok()?;
    let minute: u16 = minute.parse().ok()?;
    (hour < 24 && minute < 60 && (1..=2).contains(&text.split_once(':')?.0.len()))
        .then_some((hour, minute))
}

fn utc_system_time(at: Timestamp) -> Option<SYSTEMTIME> {
    let text = at.to_string();
    let field = |range: std::ops::Range<usize>| text.get(range)?.parse::<u16>().ok();
    Some(SYSTEMTIME {
        wYear: field(0..4)?,
        wMonth: field(5..7)?,
        wDayOfWeek: 0,
        wDay: field(8..10)?,
        wHour: field(11..13)?,
        wMinute: field(14..16)?,
        wSecond: field(17..19)?,
        wMilliseconds: 0,
    })
}

fn to_local(at: Timestamp) -> Option<SYSTEMTIME> {
    let utc = utc_system_time(at)?;
    let mut local = utc;
    // SAFETY: both pointers are valid SYSTEMTIME values for the call; a null
    // time zone means the current one.
    let ok = unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) };
    (ok != 0).then_some(local)
}

fn from_local(year: u16, month: u16, day: u16, hour: u16, minute: u16) -> Option<Timestamp> {
    let local = SYSTEMTIME {
        wYear: year,
        wMonth: month,
        wDayOfWeek: 0,
        wDay: day,
        wHour: hour,
        wMinute: minute,
        wSecond: 0,
        wMilliseconds: 0,
    };
    let mut utc = local;
    // SAFETY: as above. The call validates the date and fails on a bad one.
    let ok = unsafe { TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) };
    if ok == 0 {
        return None;
    }
    Timestamp::parse(&format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        utc.wYear, utc.wMonth, utc.wDay, utc.wHour, utc.wMinute, utc.wSecond
    ))
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_and_utc_times_parse_exactly() {
        let now = Timestamp::parse("2026-09-24T10:00:00Z").unwrap();
        assert_eq!(
            parse("+30m", now).unwrap(),
            Timestamp::parse("2026-09-24T10:30:00Z").unwrap()
        );
        assert_eq!(
            parse("+1d", now).unwrap(),
            Timestamp::parse("2026-09-25T10:00:00Z").unwrap()
        );
        assert_eq!(
            parse("2026-10-01T08:00:00Z", now).unwrap(),
            Timestamp::parse("2026-10-01T08:00:00Z").unwrap()
        );
        for bad in [
            "+5x",
            "+",
            "25:00",
            "12:60",
            "tomorrow",
            "2026-13-01 10:00",
            "+-5m",
        ] {
            assert!(parse(bad, now).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_clock_time_is_the_next_time_the_clock_shows_it() {
        let now = Timestamp::now();
        let at = parse("00:00", now).unwrap();
        let ahead = at.unix_ms() - now.unix_ms();
        assert!(ahead > 0 && ahead <= 25 * 3_600_000, "{ahead}");
        assert!(local(at).ends_with("00:00"));
        let dated = parse("2030-01-02 03:04", now).unwrap();
        assert_eq!(local(dated), "2030-01-02 03:04");
    }
}
