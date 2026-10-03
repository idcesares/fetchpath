//! Opaque identifiers and timestamps (job contract §2).
//!
//! Every identifier is a lowercase UUID string. Each kind is its own type so
//! the compiler rejects passing a command id where a job id belongs.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

const UUID_PATTERN: &str = "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

/// True for a lowercase, hyphenated UUID, the only accepted spelling.
fn is_lower_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}

macro_rules! uuid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// A fresh random identifier.
            pub fn random() -> Self {
                Self(uuid::Uuid::new_v4().to_string())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = String;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                if is_lower_uuid(&value) {
                    Ok(Self(value))
                } else {
                    Err(format!(
                        "{} must be a lowercase UUID",
                        stringify!($name)
                    ))
                }
            }
        }

        impl TryFrom<&str> for $name {
            type Error = String;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_from(value.to_owned())
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "format": "uuid",
                    "pattern": UUID_PATTERN
                })
            }
        }
    };
}

uuid_id!(
    /// One connection's namespace for command ids. A client keeps it for as
    /// long as it may retry a command.
    ClientId
);
uuid_id!(
    /// Idempotency key for one command within a client's namespace.
    CommandId
);
uuid_id!(
    /// A job, stable from creation through history retention.
    JobId
);
uuid_id!(
    /// One attempt at work for a job. Never reused.
    AttemptId
);
uuid_id!(
    /// One engine's data folder: the queue a client means to act on
    /// (contract D6). Created once and never changed; the name shown beside
    /// it is a setting.
    InstanceId
);
uuid_id!(
    /// A stored protected request context, such as a browser capture's
    /// cookies. The secret itself never crosses the protocol.
    CredentialRef
);

/// A UTC instant as an RFC 3339 string ending in `Z`, such as
/// `2026-09-20T12:00:00Z` or `2026-09-20T12:00:00.250Z`.
///
/// Timestamps describe; they never order work. Ordering uses sequence numbers.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Timestamp {
    unix_ms: i64,
}

impl Timestamp {
    pub fn now() -> Self {
        let unix_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
            Err(before) => -i64::try_from(before.duration().as_millis()).unwrap_or(i64::MAX),
        };
        Self { unix_ms }
    }

    pub fn from_unix_ms(unix_ms: i64) -> Self {
        Self { unix_ms }
    }

    pub fn unix_ms(self) -> i64 {
        self.unix_ms
    }

    /// Parses the strict form this crate writes: `YYYY-MM-DDTHH:MM:SS`, an
    /// optional fraction of up to nine digits (kept to the millisecond), and
    /// `Z`. Offsets other than `Z` are refused so every peer compares the
    /// same instant the same way.
    pub fn parse(text: &str) -> Result<Self, String> {
        let invalid = || format!("{text:?} is not an RFC 3339 UTC timestamp");
        let bytes = text.as_bytes();
        if bytes.len() < 20 || bytes.len() > 30 || !text.is_ascii() || !text.ends_with('Z') {
            return Err(invalid());
        }
        let number = |range: std::ops::Range<usize>| -> Result<i64, String> {
            let part = &text[range];
            if part.bytes().all(|byte| byte.is_ascii_digit()) {
                part.parse::<i64>().map_err(|_| invalid())
            } else {
                Err(invalid())
            }
        };
        if bytes[4] != b'-'
            || bytes[7] != b'-'
            || bytes[10] != b'T'
            || bytes[13] != b':'
            || bytes[16] != b':'
        {
            return Err(invalid());
        }
        let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
        let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
        let millis = match &text[19..text.len() - 1] {
            "" => 0,
            fraction => {
                let digits = fraction.strip_prefix('.').ok_or_else(invalid)?;
                if digits.is_empty()
                    || digits.len() > 9
                    || !digits.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(invalid());
                }
                let padded = format!("{digits:0<3}");
                padded[..3].parse::<i64>().map_err(|_| invalid())?
            }
        };
        if !(1..=12).contains(&month)
            || day < 1
            || day > days_in_month(year, month)
            || hour > 23
            || minute > 59
            || second > 59
        {
            return Err(invalid());
        }
        let days = days_from_civil(year, month, day);
        Ok(Self {
            unix_ms: ((days * 24 + hour) * 60 + minute) * 60_000 + second * 1_000 + millis,
        })
    }
}

impl TryFrom<String> for Timestamp {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<Timestamp> for String {
    fn from(value: Timestamp) -> Self {
        value.to_string()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let days = self.unix_ms.div_euclid(86_400_000);
        let in_day = self.unix_ms.rem_euclid(86_400_000);
        let (year, month, day) = civil_from_days(days);
        let (hour, minute) = (in_day / 3_600_000, in_day / 60_000 % 60);
        let (second, millis) = (in_day / 1_000 % 60, in_day % 1_000);
        write!(
            formatter,
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
        )?;
        if millis != 0 {
            write!(formatter, ".{millis:03}")?;
        }
        formatter.write_str("Z")
    }
}

impl JsonSchema for Timestamp {
    fn schema_name() -> Cow<'static, str> {
        "Timestamp".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "date-time",
            "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?Z$"
        })
    }
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_accept_only_lowercase_uuids() {
        let id = JobId::random();
        assert_eq!(JobId::try_from(id.as_str()).unwrap(), id);
        assert!(JobId::try_from("018F9C2A-0D55-74CC-B6C0-7CC8B1C9F221").is_err());
        assert!(JobId::try_from("018f9c2a0d5574ccb6c07cc8b1c9f221").is_err());
        assert!(JobId::try_from("not-a-uuid").is_err());
        assert!(serde_json::from_str::<CommandId>("\"x\"").is_err());
    }

    #[test]
    fn timestamps_round_trip_through_the_strict_utc_form() {
        for text in [
            "1970-01-01T00:00:00Z",
            "2026-09-20T12:00:00Z",
            "2026-09-20T12:00:04.250Z",
            "2000-02-29T23:59:59.999Z",
            "1969-12-31T23:59:59Z",
        ] {
            let parsed = Timestamp::parse(text).unwrap();
            assert_eq!(parsed.to_string(), text);
        }
        assert_eq!(
            Timestamp::parse("2026-09-20T12:00:00Z").unwrap().unix_ms(),
            1_789_905_600_000
        );
        assert_eq!(
            Timestamp::parse("2026-09-20T12:00:00.5Z")
                .unwrap()
                .to_string(),
            "2026-09-20T12:00:00.500Z"
        );
        for bad in [
            "2026-09-20T12:00:00+01:00",
            "2026-09-20 12:00:00Z",
            "2026-02-30T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "2026-09-20T24:00:00Z",
            "2026-09-20T12:00:00.Z",
            "2026-09-20T12:00:00.1234567890Z",
            "+026-09-20T12:00:00Z",
        ] {
            assert!(Timestamp::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn now_formats_as_a_parseable_timestamp() {
        let now = Timestamp::now();
        assert_eq!(Timestamp::parse(&now.to_string()).unwrap(), now);
    }
}
