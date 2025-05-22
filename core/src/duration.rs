/// A custom `Duration` type that wraps `std::time::Duration` and always serializes/deserializes
/// to and from the string format `HH:MM:SS.mmm`, where:
/// - `HH` is hours (zero-padded, can be more than two digits)
/// - `MM` is minutes (zero-padded, 00-59)
/// - `SS` is seconds (zero-padded, 00-59)
/// - `mmm` is milliseconds (zero-padded, 000-999)
use std::{fmt, time};

use derive_more::{Deref, From, Into};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, From, Into, Deref)]
pub struct Duration(time::Duration);

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total_seconds = self.0.as_secs();
        let millis = self.0.subsec_millis();
        let hours = total_seconds / 3600;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;
        write!(
            f,
            "{:02}:{:02}:{:02}.{:03}",
            hours, minutes, seconds, millis
        )
    }
}

impl Serialize for Duration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let parts: Vec<&str> = s.split([':', '.']).collect();
        if parts.len() != 4 {
            return Err(serde::de::Error::custom("Invalid duration format"));
        }
        let hours: u64 = parts[0].parse().map_err(serde::de::Error::custom)?;
        let minutes: u64 = parts[1].parse().map_err(serde::de::Error::custom)?;
        let seconds: u64 = parts[2].parse().map_err(serde::de::Error::custom)?;
        let millis: u32 = parts[3].parse().map_err(serde::de::Error::custom)?;
        Ok(Duration(time::Duration::new(
            hours * 3600 + minutes * 60 + seconds,
            millis * 1_000_000,
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::time;

    use super::*;

    #[test]
    fn custom_duration_serializes_correctly() {
        let duration = Duration(time::Duration::new(3661, 123_000_000));
        let serialized = serde_json::to_string(&duration).unwrap();
        assert_eq!(serialized, "\"01:01:01.123\"");
    }

    #[test]
    fn custom_duration_deserializes_correctly() {
        let serialized = "\"01:01:01.123\"";
        let deserialized: Duration = serde_json::from_str(serialized).unwrap();
        assert_eq!(
            deserialized,
            Duration(time::Duration::new(3661, 123_000_000))
        );
    }

    #[test]
    fn custom_duration_zero_case() {
        let duration = Duration(time::Duration::new(0, 0));
        let serialized = serde_json::to_string(&duration).unwrap();
        assert_eq!(serialized, "\"00:00:00.000\"");

        let deserialized: Duration = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, Duration(time::Duration::new(0, 0)));
    }

    #[test]
    fn custom_duration_large_hours() {
        let duration = Duration(time::Duration::new(360000, 456_000_000)); // 100 hours, 0 minutes, 0 seconds, and 456 milliseconds
        let serialized = serde_json::to_string(&duration).unwrap();
        assert_eq!(serialized, "\"100:00:00.456\"");

        let deserialized: Duration = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            deserialized,
            Duration(time::Duration::new(360000, 456_000_000))
        );
    }
}
