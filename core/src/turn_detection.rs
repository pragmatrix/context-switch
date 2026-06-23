use serde::{Deserialize, Serialize};

/// Provider-neutral turn-detection configuration.
///
/// Each backend converter forwards only the fields it understands and ignores the rest.
/// To avoid lossy numeric-to-categorical mapping, the end-of-turn aggressiveness is
/// expressed twice with distinct, provider-specific shapes:
/// - `threshold` (and `eager_threshold`) are confidence floats consumed by Deepgram Flux.
/// - `threshold_level` is the categorical level consumed by Voice Live.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDetection {
    /// End-of-turn confidence threshold (Deepgram Flux `eot_threshold`, valid range `0.5`–`0.9`).
    /// Ignored by Voice Live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// Categorical end-of-turn aggressiveness (Voice Live `threshold_level`).
    /// Ignored by Deepgram.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold_level: Option<ThresholdLevel>,
    /// Silence (ms) before a turn is force-closed. Forwarded by both backends.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
    /// Eager (early) end-of-turn confidence threshold (Deepgram Flux `eager_eot_threshold`,
    /// valid range `0.3`–`0.9`). Ignored by Voice Live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eager_threshold: Option<f64>,
}

/// Categorical end-of-turn aggressiveness consumed by Voice Live.
///
/// Lower levels wait for stronger evidence before ending a turn; higher levels end turns
/// earlier. When unset, Voice Live uses its built-in default behavior.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThresholdLevel {
    Low,
    Medium,
    High,
}
