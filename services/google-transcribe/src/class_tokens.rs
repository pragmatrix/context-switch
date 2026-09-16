//! Google Cloud Speech-to-Text V2 class tokens for speech adaptation.
//!
//! Class tokens are placeholders that can be embedded in `PhraseSet` phrases to
//! bias the recognizer toward a whole class of values (numbers, dates, phone
//! numbers, ...) without enumerating every possible value. They are sent inside
//! an inline `PhraseSet` via `RecognitionConfig.adaptation`.
//!
//! Token availability varies by locale and transcription model; Google
//! silently ignores tokens that are not supported for the request's locale.
//! The authoritative per-locale table is published at
//! <https://docs.cloud.google.com/speech-to-text/docs/class-tokens>.
//!
//! Two naming families exist: the `$OOV_CLASS_*` prefixed tokens and the bare
//! `$*` tokens. Seven base names exist in both forms; the two families are not
//! interchangeable.

/// A sequence of letters `[a-z]` and/or digits, for example "a1b2c3".
pub const OOV_CLASS_ALPHANUMERIC_SEQUENCE: &str = "$OOV_CLASS_ALPHANUMERIC_SEQUENCE";

/// A sequence of letters `[a-z]`, for example "cqbcf".
pub const OOV_CLASS_ALPHA_SEQUENCE: &str = "$OOV_CLASS_ALPHA_SEQUENCE";

/// An AM radio frequency, for example "twelve twenty" → `1220`.
pub const OOV_CLASS_AM_RADIO_FREQUENCY: &str = "$OOV_CLASS_AM_RADIO_FREQUENCY";

/// A digit sequence of any length, for example "nine four one two" → `9412`.
pub const OOV_CLASS_DIGIT_SEQUENCE: &str = "$OOV_CLASS_DIGIT_SEQUENCE";

/// An FM radio frequency, for example "one oh four point three" → `104.3`.
pub const OOV_CLASS_FM_RADIO_FREQUENCY: &str = "$OOV_CLASS_FM_RADIO_FREQUENCY";

/// A street number for an address in the target locale (prefixed variant),
/// for example "one hundred ninety one" → `191`.
pub const OOV_CLASS_ADDRESSNUM: &str = "$OOV_CLASS_ADDRESSNUM";

/// A full date using numbers (prefixed variant), for example
/// "nine nine nine two thousand fourteen" → `9.9.2014` (locale-dependent format).
pub const OOV_CLASS_FULLDATE: &str = "$OOV_CLASS_FULLDATE";

/// A phone number as used in the target locale (prefixed variant), for example
/// "six five oh five five five six one oh one" → `650-555-6101`.
pub const OOV_CLASS_FULLPHONENUM: &str = "$OOV_CLASS_FULLPHONENUM";

/// A numerical value including whole numbers, fractions, and decimals
/// (prefixed variant), for example "twenty two" → `22`.
pub const OOV_CLASS_OPERAND: &str = "$OOV_CLASS_OPERAND";

/// An ordinal number (prefixed variant), for example "third" → `3rd`.
pub const OOV_CLASS_ORDINAL: &str = "$OOV_CLASS_ORDINAL";

/// A percentage value including the percent sign (prefixed variant), for
/// example "ten point five percent" → `10.5%`.
pub const OOV_CLASS_PERCENT: &str = "$OOV_CLASS_PERCENT";

/// A postal code as used in the target locale (prefixed variant), for example
/// "one zero zero one zero" → `10010`.
pub const OOV_CLASS_POSTALCODE: &str = "$OOV_CLASS_POSTALCODE";

/// A temperature in degrees, for example "minus one" → `-1`.
pub const OOV_CLASS_TEMPERATURE: &str = "$OOV_CLASS_TEMPERATURE";

/// A television channel number, for example "two zero two" → `202`.
pub const OOV_CLASS_TV_CHANNEL: &str = "$OOV_CLASS_TV_CHANNEL";

/// A street number for an address in the target locale, for example
/// "one hundred ninety one" → `191`.
pub const ADDRESSNUM: &str = "$ADDRESSNUM";

/// A full date using numbers, for example "nine nine nine two thousand
/// fourteen" → `9.9.2014` (locale-dependent format).
pub const FULLDATE: &str = "$FULLDATE";

/// A phone number as used in the target locale, for example
/// "one eight hundred five five five four oh oh one" → `+1-800-555-4001`.
pub const FULLPHONENUM: &str = "$FULLPHONENUM";

/// A numerical value including whole numbers, fractions, and decimals, for
/// example "twenty two" → `22`.
pub const OPERAND: &str = "$OPERAND";

/// An ordinal number, for example "third" → `3rd`.
pub const ORDINAL: &str = "$ORDINAL";

/// A percentage value including the percent sign, for example
/// "ten point five percent" → `10.5%`.
pub const PERCENT: &str = "$PERCENT";

/// A postal code as used in the target locale, for example
/// "one zero zero one zero" → `10010`.
pub const POSTALCODE: &str = "$POSTALCODE";

/// A numbered day within a month, for example "the twenty third" → `23rd`.
pub const DAY: &str = "$DAY";

/// An amount of money with a currency unit name, for example
/// "forty three dollars" → `$43`.
pub const MONEY: &str = "$MONEY";

/// A named month in a year, for example "july" → `July`. Contextual phrases
/// like "2 months from now" are not supported.
pub const MONTH: &str = "$MONTH";

/// A numbered street name, for example "fifty first" → `51st`.
pub const STREET: &str = "$STREET";

/// A specific time of day, for example "ten thirty" → `10:30`.
pub const TIME: &str = "$TIME";

/// A year, for example "twenty ten" → `2010`.
pub const YEAR: &str = "$YEAR";
