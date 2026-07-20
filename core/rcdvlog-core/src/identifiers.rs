use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

const MAX_IDENTIFIER_BYTES: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentifierErrorReason {
    Empty,
    TooLong,
    ContainsWhitespace,
    ContainsControlCharacter,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize)]
#[error("invalid {identifier_kind}: {reason}")]
#[serde(rename_all = "camelCase")]
pub struct IdentifierError {
    pub code: &'static str,
    pub identifier_kind: &'static str,
    pub reason: IdentifierErrorReason,
}

impl fmt::Display for IdentifierErrorReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "identifier cannot be empty",
            Self::TooLong => "identifier cannot exceed 255 UTF-8 bytes",
            Self::ContainsWhitespace => "identifier cannot contain whitespace",
            Self::ContainsControlCharacter => "identifier cannot contain control characters",
        })
    }
}

fn validate_identifier(
    identifier_kind: &'static str,
    value: String,
) -> Result<String, IdentifierError> {
    let reason = if value.is_empty() {
        Some(IdentifierErrorReason::Empty)
    } else if value.len() > MAX_IDENTIFIER_BYTES {
        Some(IdentifierErrorReason::TooLong)
    } else if value.chars().any(char::is_control) {
        Some(IdentifierErrorReason::ContainsControlCharacter)
    } else if value.chars().any(char::is_whitespace) {
        Some(IdentifierErrorReason::ContainsWhitespace)
    } else {
        None
    };
    match reason {
        Some(reason) => Err(IdentifierError {
            code: "invalid_identifier",
            identifier_kind,
            reason,
        }),
        None => Ok(value),
    }
}

macro_rules! identifier {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub const KIND: &'static str = $kind;

            pub fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
                validate_identifier(Self::KIND, value.into()).map(Self)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = IdentifierError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdentifierError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = IdentifierError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(de::Error::custom)
            }
        }
    };
}

identifier!(NodeId, "nodeId");
identifier!(ProjectId, "projectId");
identifier!(ProjectAssetId, "projectAssetId");
identifier!(MediaObjectId, "mediaObjectId");
identifier!(StorageRootId, "storageRootId");
identifier!(StorageObjectId, "storageObjectId");
identifier!(RepresentationId, "representationId");
identifier!(OperationId, "operationId");
identifier!(JobId, "jobId");

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identifiers_are_opaque_validated_json_strings() {
        let project = ProjectId::parse("project_existing_123").unwrap();
        assert_eq!(project.as_str(), "project_existing_123");
        assert_eq!(
            serde_json::to_value(&project).unwrap(),
            json!("project_existing_123")
        );
        assert_eq!(
            serde_json::from_value::<ProjectId>(json!("project_existing_123")).unwrap(),
            project
        );

        let error = NodeId::parse("node with spaces").unwrap_err();
        assert_eq!(error.code, "invalid_identifier");
        assert_eq!(error.identifier_kind, "nodeId");
        assert_eq!(error.reason, IdentifierErrorReason::ContainsWhitespace);
        assert!(MediaObjectId::parse("").is_err());
        assert!(JobId::parse("job_\0bad").is_err());
    }
}
