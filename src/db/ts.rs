//! Stored timestamps, for `#[serde(with = "ts")]` and `#[serde(with = "ts::option")]`.
//!
//! Times are written as fixed-width RFC 3339 UTC with millisecond precision,
//! so string order is time order, and read back from any RFC 3339 form.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serializer};

pub fn format(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn parse(s: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(s).map(|t| t.to_utc())
}

#[allow(clippy::trivially_copy_pass_by_ref)]
pub fn serialize<S: Serializer>(ts: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format(*ts))
}

pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
    parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
}

pub mod option {
    use super::{DateTime, Deserialize, Deserializer, Serializer, Utc};

    #[allow(clippy::ref_option)]
    pub fn serialize<S: Serializer>(ts: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
        match ts {
            Some(t) => super::serialize(t, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<DateTime<Utc>>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|s| super::parse(&s).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatted_times_sort_chronologically_and_round_trip() {
        let a = parse("2026-05-26T09:00:00Z").unwrap();
        let b = parse("2026-05-26T10:00:00.5+01:00").unwrap() + chrono::Duration::hours(1);
        assert_eq!(format(a), "2026-05-26T09:00:00.000Z");
        assert_eq!(format(b), "2026-05-26T10:00:00.500Z");
        assert!(format(a) < format(b));
        assert_eq!(parse(&format(b)).unwrap(), b);
    }
}
