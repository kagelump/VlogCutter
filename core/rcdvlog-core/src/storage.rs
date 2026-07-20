use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

use crate::NodeId;

const MAX_RELATIVE_PATH_BYTES: usize = 4096;

/// A protocol-level path relative to a logical storage root.
///
/// `/` is the only separator. Ordinary percent text is preserved, while
/// security-sensitive percent escapes that could become traversal, separators,
/// controls, drive syntax, or nested escapes are rejected.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RootRelativePath(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelativePathErrorReason {
    Empty,
    TooLong,
    Absolute,
    AlternateSeparator,
    EmptyComponent,
    Traversal,
    ControlCharacter,
    EncodedComponent,
    NotCanonicalUnicode,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize)]
#[error("invalid root-relative path: {reason}")]
#[serde(rename_all = "camelCase")]
pub struct RelativePathError {
    pub code: &'static str,
    pub reason: RelativePathErrorReason,
}

impl fmt::Display for RelativePathErrorReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "path cannot be empty",
            Self::TooLong => "path cannot exceed 4096 UTF-8 bytes",
            Self::Absolute => "path must not be absolute or drive-qualified",
            Self::AlternateSeparator => "path must use only '/' separators",
            Self::EmptyComponent => "path cannot contain empty components",
            Self::Traversal => "path cannot contain '.' or '..' components",
            Self::ControlCharacter => "path cannot contain control characters",
            Self::EncodedComponent => "path contains ambiguous percent-encoded path syntax",
            Self::NotCanonicalUnicode => "path must use Unicode NFC normalization",
        })
    }
}

impl RootRelativePath {
    pub fn parse(value: impl Into<String>) -> Result<Self, RelativePathError> {
        let value = value.into();
        let reason = if value.is_empty() {
            Some(RelativePathErrorReason::Empty)
        } else if value.len() > MAX_RELATIVE_PATH_BYTES {
            Some(RelativePathErrorReason::TooLong)
        } else if value.starts_with('/')
            || value.starts_with("//")
            || value
                .as_bytes()
                .get(1)
                .is_some_and(|byte| *byte == b':' && value.as_bytes()[0].is_ascii_alphabetic())
        {
            Some(RelativePathErrorReason::Absolute)
        } else if value.contains('\\') {
            Some(RelativePathErrorReason::AlternateSeparator)
        } else if value.chars().any(char::is_control) {
            Some(RelativePathErrorReason::ControlCharacter)
        } else if contains_ambiguous_percent_escape(&value) {
            Some(RelativePathErrorReason::EncodedComponent)
        } else if value.nfc().collect::<String>() != value {
            Some(RelativePathErrorReason::NotCanonicalUnicode)
        } else if value.split('/').any(str::is_empty) {
            Some(RelativePathErrorReason::EmptyComponent)
        } else if value.split('/').any(|part| part == "." || part == "..") {
            Some(RelativePathErrorReason::Traversal)
        } else {
            None
        };
        match reason {
            Some(reason) => Err(RelativePathError {
                code: "invalid_relative_path",
                reason,
            }),
            None => Ok(Self(value)),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    pub fn is_within(&self, prefix: &Self) -> bool {
        self == prefix
            || self
                .as_str()
                .strip_prefix(prefix.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
    }

    /// Returns the binding-local suffix. An empty suffix means the logical path
    /// identifies the binding mount itself.
    pub fn suffix_after<'a>(&'a self, prefix: Option<&Self>) -> Option<&'a str> {
        match prefix {
            None => Some(self.as_str()),
            Some(prefix) if self == prefix => Some(""),
            Some(prefix) => self
                .as_str()
                .strip_prefix(prefix.as_str())
                .and_then(|suffix| suffix.strip_prefix('/')),
        }
    }
}

fn contains_ambiguous_percent_escape(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.windows(3).any(|window| {
        if window[0] != b'%' {
            return false;
        }
        let Some(high) = hex_value(window[1]) else {
            return false;
        };
        let Some(low) = hex_value(window[2]) else {
            return false;
        };
        let decoded = (high << 4) | low;
        decoded <= 0x1f || matches!(decoded, b'%' | b'.' | b'/' | b':' | b'\\' | 0x7f)
    })
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl AsRef<str> for RootRelativePath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for RootRelativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RootRelativePath {
    type Err = RelativePathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for RootRelativePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RootRelativePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }

            pub fn parse_stored(value: &str) -> Option<Self> {
                match value { $($value => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

string_enum!(StorageProtocol {
    Nfs => "nfs",
    Smb => "smb",
    Local => "local",
    Other => "other",
});

string_enum!(StorageAccessMode {
    ReadOnly => "read_only",
    ReadWrite => "read_write",
});

string_enum!(BindingVerificationStatus {
    Pending => "pending",
    Confirmed => "confirmed",
    Unavailable => "unavailable",
    Mismatch => "mismatch",
    Rejected => "rejected",
});

string_enum!(RootAssociationConfirmationStatus {
    Pending => "pending",
    Confirmed => "confirmed",
    Rejected => "rejected",
});

string_enum!(GlobalMediaVerificationState {
    Unverified => "unverified",
    Provisional => "provisional",
    Verified => "verified",
    Mismatch => "mismatch",
});

string_enum!(LocationVerificationState {
    Unverified => "unverified",
    Provisional => "provisional",
    Verified => "verified",
    Mismatch => "mismatch",
});

string_enum!(ObservationStatus {
    Accessible => "accessible",
    TemporarilyUnavailable => "temporarily_unavailable",
    FingerprintMismatch => "fingerprint_mismatch",
});

string_enum!(ObservationGenerationState {
    InProgress => "in_progress",
    Completed => "completed",
    Abandoned => "abandoned",
});

string_enum!(NodeAvailabilityState {
    Known => "known",
    AccessibleHere => "accessible_here",
    AccessibleOnAnotherNode => "accessible_on_another_node",
    TemporarilyUnavailable => "temporarily_unavailable",
    MissingAfterCompletedObservation => "missing_after_completed_observation",
    FingerprintMismatch => "fingerprint_mismatch",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootAssociationEvidence {
    pub matching_relative_paths: u64,
    pub matching_quick_fingerprints: u64,
    pub matching_full_hashes: u64,
    pub mismatches: u64,
    #[serde(default)]
    pub sampled_relative_paths: Vec<RootRelativePath>,
    pub captured_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootAssociationConfirmation {
    pub status: RootAssociationConfirmationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_by_node_id: Option<NodeId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn root_relative_paths_preserve_safe_unicode_and_spaces() {
        let path = RootRelativePath::parse("Trips/日本 2026/clip 01.MP4").unwrap();
        assert_eq!(path.as_str(), "Trips/日本 2026/clip 01.MP4");
        assert_eq!(
            serde_json::to_string(&path).unwrap(),
            "\"Trips/日本 2026/clip 01.MP4\""
        );
    }

    #[test]
    fn root_relative_paths_reject_ambiguous_or_escaping_forms() {
        for value in [
            "",
            "/absolute/file.mp4",
            "C:/windows/file.mp4",
            "server\\share\\file.mp4",
            "mixed/path\\file.mp4",
            "double//component.mp4",
            "trailing/",
            "./file.mp4",
            "parent/../file.mp4",
            "encoded/%2e%2e/file.mp4",
            "nul/\0file.mp4",
        ] {
            assert!(
                RootRelativePath::parse(value).is_err(),
                "accepted {value:?}"
            );
        }
        assert!(RootRelativePath::parse("literal/100% fun.mp4").is_ok());
        assert!(RootRelativePath::parse("literal/100%20fun.mp4").is_ok());
    }

    #[test]
    fn prefixes_compare_by_component_not_text() {
        let prefix = RootRelativePath::parse("Trips/2025").unwrap();
        let inside = RootRelativePath::parse("Trips/2025/clip.mp4").unwrap();
        let adjacent = RootRelativePath::parse("Trips/20250/clip.mp4").unwrap();
        assert!(inside.is_within(&prefix));
        assert_eq!(inside.suffix_after(Some(&prefix)), Some("clip.mp4"));
        assert!(!adjacent.is_within(&prefix));
        assert_eq!(adjacent.suffix_after(Some(&prefix)), None);
    }

    #[test]
    fn root_relative_paths_require_nfc_canonical_unicode() {
        assert!(RootRelativePath::parse("Trips/Café/file.mp4").is_ok());
        assert_eq!(
            RootRelativePath::parse("Trips/Cafe\u{301}/file.mp4")
                .unwrap_err()
                .reason,
            RelativePathErrorReason::NotCanonicalUnicode
        );
    }

    proptest! {
        #[test]
        fn safe_ascii_component_paths_round_trip(
            components in proptest::collection::vec("[A-Za-z0-9 _-]{1,24}", 1..8)
        ) {
            prop_assume!(components.iter().all(|part| part != "." && part != ".."));
            let value = components.join("/");
            let parsed = RootRelativePath::parse(value.clone()).unwrap();
            prop_assert_eq!(parsed.as_str(), value.as_str());
            let encoded = serde_json::to_string(&parsed).unwrap();
            let decoded: RootRelativePath = serde_json::from_str(&encoded).unwrap();
            prop_assert_eq!(decoded, parsed);
        }

        #[test]
        fn traversal_components_are_rejected_at_every_depth(
            prefix in proptest::collection::vec("[A-Za-z0-9_-]{1,16}", 0..6),
            traversal in prop_oneof![Just("."), Just("..")],
            suffix in proptest::collection::vec("[A-Za-z0-9_-]{1,16}", 0..6),
        ) {
            let value = prefix
                .into_iter()
                .chain(std::iter::once(traversal.to_string()))
                .chain(suffix)
                .collect::<Vec<_>>()
                .join("/");
            prop_assert_eq!(
                RootRelativePath::parse(value).unwrap_err().reason,
                RelativePathErrorReason::Traversal
            );
        }

        #[test]
        fn alternate_separators_are_never_accepted(
            left in "[A-Za-z0-9_-]{1,16}",
            right in "[A-Za-z0-9_-]{1,16}",
        ) {
            let value = format!("{left}\\\\{right}");
            prop_assert_eq!(
                RootRelativePath::parse(value).unwrap_err().reason,
                RelativePathErrorReason::AlternateSeparator
            );
        }
    }
}
