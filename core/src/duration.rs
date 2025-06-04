/// A custom `Duration` type that wraps `std::time::Duration` and always serializes/deserializes
/// to and from a floating point number representing seconds.
use std::{fmt, time};

use derive_more::{Add, AddAssign, Deref, From, Into};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, From, Into, Deref, Add, AddAssign)]
pub struct Duration(time::Duration);

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total_seconds = self.0.as_secs();
        let millis = self.0.subsec_millis();
        let hours = total_seconds / 3600;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;
        write!(f, "{}:{:02}:{:02}.{:03}", hours, minutes, seconds, millis)
    }
}

impl Serialize for Duration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Convert duration to seconds as a f64
        serializer.serialize_f64(self.0.as_secs_f64())
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let seconds: f64 = f64::deserialize(deserializer)?;

        // Use from_secs_f64 to create the Duration
        Ok(Duration(time::Duration::from_secs_f64(seconds)))
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
        // 3661.123 seconds = 1 hour, 1 minute, 1.123 seconds
        assert_eq!(serialized, "3661.123");
    }

    #[test]
    fn custom_duration_deserializes_correctly() {
        let serialized = "3661.123";
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
        assert_eq!(serialized, "0.0");

        let deserialized: Duration = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, Duration(time::Duration::new(0, 0)));
    }

    #[test]
    fn custom_duration_large_hours() {
        let duration = Duration(time::Duration::new(360000, 456_000_000)); // 100 hours, 0 minutes, 0 seconds, and 456 milliseconds
        let serialized = serde_json::to_string(&duration).unwrap();
        assert_eq!(serialized, "360000.456");

        let deserialized: Duration = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            deserialized,
            Duration(time::Duration::new(360000, 456_000_000))
        );
    }
}
