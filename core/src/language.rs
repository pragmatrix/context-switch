use std::fmt;

use derive_more::Deref;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguageListError {
    Empty,
    MultipleValues { count: usize },
}

impl fmt::Display for LanguageListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LanguageListError::Empty => write!(f, "language list must contain at least one value"),
            LanguageListError::MultipleValues { count } => {
                write!(f, "expected exactly one language value, got {count}")
            }
        }
    }
}

impl std::error::Error for LanguageListError {}

/// A non-empty list of normalized language codes.
///
/// Values are trimmed and empty entries are removed during construction.
/// Construction fails if no non-empty values remain.
#[derive(Debug, Clone, PartialEq, Eq, Deref)]
pub struct Languages(Vec<String>);

impl Languages {
    /// Creates a non-empty language list from raw values.
    pub fn new(values: Vec<String>) -> Result<Self, LanguageListError> {
        let values = values
            .into_iter()
            .map(|x| x.trim().to_owned())
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>();

        if values.is_empty() {
            return Err(LanguageListError::Empty);
        }

        Ok(Self(values))
    }

    /// Creates a non-empty language list from a comma-separated string.
    pub fn from_csv(language: &str) -> Result<Self, LanguageListError> {
        Self::new(
            language
                .split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    }

    /// Returns the first language code.
    ///
    /// The list is guaranteed to be non-empty after construction.
    pub fn first(&self) -> &String {
        &self.0[0]
    }

    /// Returns the single language code.
    ///
    /// Fails if multiple language codes are present.
    pub fn single(&self) -> Result<&String, LanguageListError> {
        if self.0.len() == 1 {
            Ok(self.first())
        } else {
            Err(LanguageListError::MultipleValues {
                count: self.0.len(),
            })
        }
    }

    /// Returns the language codes joined by commas.
    pub fn join_csv(&self) -> String {
        self.0.join(",")
    }
}

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

/// Converts an ISO 639 language code into a BCP 47 language tag.
///
/// The conversion returns a primary language tag only. If a matching ISO 639-1 code exists,
/// that 2-letter code is preferred (for example `eng` -> `en`). Otherwise the original ISO
/// 639-3 code is used as the BCP 47 primary language subtag.
///
/// Supports ISO 639-1 (2-letter) and ISO 639-3 (3-letter) input codes.
pub fn iso639_to_bcp47(code: &str) -> Result<String, LanguageCodeError> {
    let language = match code.len() {
        2 => Language::from_639_1(code),
        3 => Language::from_639_3(code),
        _ => None,
    }
    .ok_or_else(|| LanguageCodeError::UnsupportedLanguage {
        language: code.to_string(),
    })?;

    Ok(language
        .to_639_1()
        .map(str::to_string)
        .unwrap_or_else(|| language.to_639_3().to_string()))
}

/// Converts an ISO 639-3 language code into a BCP 47 language tag.
pub fn iso639_3_to_bcp47(code: &str) -> Result<String, LanguageCodeError> {
    iso639_to_bcp47(code)
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
