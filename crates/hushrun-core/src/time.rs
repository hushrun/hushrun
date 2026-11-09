//! Timestamps and durations.
//!
//! Events carry RFC 3339 UTC timestamps with milliseconds (`2026-10-13T15:04:05.123Z`, E-4).
//! Policies write durations such as `30s`, `20m`, `3h`, `24h`, `7d` or `1h30m`.

use std::fmt;
use std::str::FromStr;

use jiff::fmt::temporal::DateTimePrinter;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A point in time, with millisecond precision.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time(jiff::Timestamp);

/// Why a string is not a timestamp or a duration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("{0:?} is not an RFC 3339 timestamp")]
    Timestamp(String),
    #[error("{0:?} is not a duration such as 30s, 20m, 3h, 7d or 1h30m")]
    Duration(String),
    #[error("the time is out of range")]
    Range,
}

impl Time {
    /// From milliseconds since the Unix epoch.
    pub fn from_millis(ms: i64) -> Result<Time, TimeError> {
        jiff::Timestamp::from_millisecond(ms)
            .map(Time)
            .map_err(|_| TimeError::Range)
    }

    /// Milliseconds since the Unix epoch.
    pub fn millis(self) -> i64 {
        self.0.as_millisecond()
    }

    /// Parses any RFC 3339 timestamp, truncated to milliseconds.
    pub fn parse(text: &str) -> Result<Time, TimeError> {
        let ts: jiff::Timestamp = text
            .parse()
            .map_err(|_| TimeError::Timestamp(text.to_string()))?;
        Time::from_millis(ts.as_millisecond())
    }

    pub fn checked_add(self, d: Dur) -> Option<Time> {
        let ms = self.millis().checked_add(i64::try_from(d.millis()).ok()?)?;
        Time::from_millis(ms).ok()
    }

    pub fn checked_sub(self, d: Dur) -> Option<Time> {
        let ms = self.millis().checked_sub(i64::try_from(d.millis()).ok()?)?;
        Time::from_millis(ms).ok()
    }

    /// `self + d`, or the latest representable time.
    pub fn saturating_add(self, d: Dur) -> Time {
        self.checked_add(d).unwrap_or(Time(jiff::Timestamp::MAX))
    }

    /// Milliseconds from `earlier` to `self`; negative if `earlier` is later.
    pub fn millis_since(self, earlier: Time) -> i64 {
        self.millis() - earlier.millis()
    }

    /// The jiff timestamp, for IO-side code.
    pub fn timestamp(self) -> jiff::Timestamp {
        self.0
    }

    pub fn from_timestamp(ts: jiff::Timestamp) -> Time {
        Time::from_millis(ts.as_millisecond()).expect("a jiff timestamp is in range")
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            &DateTimePrinter::new()
                .precision(Some(3))
                .timestamp_to_string(&self.0),
        )
    }
}

impl fmt::Debug for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Time {
    type Err = TimeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Time::parse(s)
    }
}

impl Serialize for Time {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Time {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Time::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// A duration, with millisecond precision.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dur(u64);

impl Dur {
    pub const ZERO: Dur = Dur(0);

    pub const fn from_millis(ms: u64) -> Dur {
        Dur(ms)
    }

    pub const fn from_secs(s: u64) -> Dur {
        Dur(s * 1000)
    }

    pub const fn millis(self) -> u64 {
        self.0
    }

    pub fn as_std(self) -> std::time::Duration {
        std::time::Duration::from_millis(self.0)
    }

    /// Parses `<number><unit>` parts, units `ms`, `s`, `m`, `h`, `d` (24 h) and `w` (7 d),
    /// largest first, such as `90s`, `20m`, `1h30m` or `7d`.
    pub fn parse(text: &str) -> Result<Dur, TimeError> {
        let err = || TimeError::Duration(text.to_string());
        if text.is_empty() {
            return Err(err());
        }
        let mut total: u64 = 0;
        let mut rest = text;
        let mut last_unit = u64::MAX;
        while !rest.is_empty() {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return Err(err());
            }
            let value: u64 = rest[..digits].parse().map_err(|_| err())?;
            rest = &rest[digits..];
            let unit_len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
            let unit = match &rest[..unit_len] {
                "ms" => 1,
                "s" => 1_000,
                "m" => 60_000,
                "h" => 3_600_000,
                "d" => 86_400_000,
                "w" => 604_800_000,
                _ => return Err(err()),
            };
            if unit >= last_unit {
                return Err(err());
            }
            last_unit = unit;
            rest = &rest[unit_len..];
            total = value
                .checked_mul(unit)
                .and_then(|v| total.checked_add(v))
                .ok_or_else(err)?;
        }
        Ok(Dur(total))
    }
}

impl fmt::Display for Dur {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut ms = self.0;
        if ms == 0 {
            return f.write_str("0s");
        }
        for (unit, name) in [
            (86_400_000, "d"),
            (3_600_000, "h"),
            (60_000, "m"),
            (1_000, "s"),
            (1, "ms"),
        ] {
            if ms >= unit {
                write!(f, "{}{name}", ms / unit)?;
                ms %= unit;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for Dur {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Dur {
    type Err = TimeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Dur::parse(s)
    }
}

impl Serialize for Dur {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Dur::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: E-4
    #[test]
    fn timestamps_print_utc_with_milliseconds() {
        let t = Time::parse("2026-10-13T15:04:05.123Z").unwrap();
        assert_eq!(t.to_string(), "2026-10-13T15:04:05.123Z");
        assert_eq!(
            Time::parse("2026-10-13T15:04:05Z").unwrap().to_string(),
            "2026-10-13T15:04:05.000Z"
        );
        assert_eq!(
            Time::parse("2026-10-13T17:04:05.1+02:00")
                .unwrap()
                .to_string(),
            "2026-10-13T15:04:05.100Z"
        );
        // Sub-millisecond digits are dropped.
        assert_eq!(Time::parse("2026-10-13T15:04:05.123999Z").unwrap(), t);
        assert!(Time::parse("2026-10-13 15:04:05").is_err());
        assert_eq!(Time::from_millis(t.millis()).unwrap(), t);
    }

    // covers: C-5, C-7, C-8
    #[test]
    fn durations() {
        assert_eq!(Dur::parse("30s").unwrap(), Dur::from_secs(30));
        assert_eq!(Dur::parse("20m").unwrap(), Dur::from_secs(1200));
        assert_eq!(Dur::parse("3h").unwrap(), Dur::from_secs(3 * 3600));
        assert_eq!(Dur::parse("7d").unwrap(), Dur::from_secs(7 * 86400));
        assert_eq!(Dur::parse("1h30m").unwrap(), Dur::from_secs(5400));
        assert_eq!(Dur::parse("1w").unwrap().to_string(), "7d");
        assert_eq!(Dur::parse("90s").unwrap().to_string(), "1m30s");
        assert_eq!(Dur::parse("250ms").unwrap().millis(), 250);
        for bad in [
            "",
            "10",
            "s",
            "1x",
            "1m1h",
            "1h1h",
            "-1s",
            "1.5h",
            " 1s",
            "99999999999999999999d",
        ] {
            assert!(Dur::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn arithmetic() {
        let t = Time::parse("2026-10-13T15:04:05.123Z").unwrap();
        let later = t.checked_add(Dur::parse("3h").unwrap()).unwrap();
        assert_eq!(later.to_string(), "2026-10-13T18:04:05.123Z");
        assert_eq!(later.millis_since(t), 3 * 3_600_000);
        assert_eq!(later.checked_sub(Dur::parse("3h").unwrap()).unwrap(), t);
    }
}
