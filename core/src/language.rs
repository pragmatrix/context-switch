use std::fmt;

use isolang::Language;
use oxilangtag::LanguageTag;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguageCodeError {
    InvalidBcp47Tag { tag: String, message: String },
    UnsupportedLanguage { language: String },
}

impl fmt::Display for LanguageCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LanguageCodeError::InvalidBcp47Tag { tag, message } => {
                write!(f, "Invalid BCP 47 tag '{tag}': {message}")
            }
            LanguageCodeError::UnsupportedLanguage { language } => {
                write!(f, "Unsupported language subtag '{language}'")
            }
        }
    }
}

impl std::error::Error for LanguageCodeError {}

/// Converts a BCP 47 language tag into its ISO 639-3 language code.
///
/// The conversion uses the primary language subtag only and ignores script, region, variant,
/// and extension subtags.
pub fn bcp47_to_iso639_3(tag: &str) -> Result<&'static str, LanguageCodeError> {
    let parsed = LanguageTag::parse(tag).map_err(|error| LanguageCodeError::InvalidBcp47Tag {
        tag: tag.to_string(),
        message: error.to_string(),
    })?;

    let primary_language = parsed.primary_language();
    let language = match primary_language.len() {
        2 => Language::from_639_1(primary_language),
        3 => Language::from_639_3(primary_language),
        _ => None,
    };

    language
        .map(|x| x.to_639_3())
        .ok_or_else(|| LanguageCodeError::UnsupportedLanguage {
            language: primary_language.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bcp47_to_iso639_3_for_primary_language_tags() {
        assert_eq!(bcp47_to_iso639_3("en").unwrap(), "eng");
        assert_eq!(bcp47_to_iso639_3("de").unwrap(), "deu");
        assert_eq!(bcp47_to_iso639_3("fr").unwrap(), "fra");
    }

    #[test]
    fn bcp47_to_iso639_3_ignores_non_primary_subtags() {
        assert_eq!(bcp47_to_iso639_3("en-US").unwrap(), "eng");
        assert_eq!(bcp47_to_iso639_3("zh-Hant-TW").unwrap(), "zho");
    }

    #[test]
    fn bcp47_to_iso639_3_rejects_malformed_tags() {
        let err = bcp47_to_iso639_3("en--US").unwrap_err();
        assert!(matches!(err, LanguageCodeError::InvalidBcp47Tag { .. }));
    }

    #[test]
    fn bcp47_to_iso639_3_rejects_unsupported_primary_language() {
        let err = bcp47_to_iso639_3("qaa").unwrap_err();
        assert_eq!(
            err,
            LanguageCodeError::UnsupportedLanguage {
                language: "qaa".to_string(),
            }
        );
    }
}
