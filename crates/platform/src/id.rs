//! Stable, validated identifiers.
//!
//! Every contribution (module, command, view, document kind) is addressed by a
//! dotted lowercase identifier such as `retrowork.logs` or `logs.filter.focus`.
//! Identifiers are part of the public contract: they appear in package
//! manifests, persisted state, key bindings and UI automation selectors, so
//! they are validated once at the boundary and never built from display text.

use std::{fmt, str::FromStr, sync::Arc};

use serde::{Deserialize, Serialize};

/// Longest accepted identifier, in bytes.
pub const MAX_ID_LEN: usize = 128;

/// Why a string is not a valid identifier.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("identifier is empty")]
    Empty,
    #[error("identifier `{0}` is longer than {MAX_ID_LEN} bytes")]
    TooLong(String),
    #[error("identifier `{0}` has an empty segment")]
    EmptySegment(String),
    #[error(
        "identifier `{id}` contains `{found}`; allowed: a-z, 0-9, `-`, `_`, with `.` between segments"
    )]
    InvalidCharacter { id: String, found: char },
    #[error("identifier `{0}` has a segment that does not start with a letter or digit")]
    InvalidSegmentStart(String),
}

fn validate(value: &str) -> Result<(), IdError> {
    if value.is_empty() {
        return Err(IdError::Empty);
    }
    if value.len() > MAX_ID_LEN {
        return Err(IdError::TooLong(value.to_owned()));
    }
    for segment in value.split('.') {
        let Some(first) = segment.chars().next() else {
            return Err(IdError::EmptySegment(value.to_owned()));
        };
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            if first == '-' || first == '_' {
                return Err(IdError::InvalidSegmentStart(value.to_owned()));
            }
            return Err(IdError::InvalidCharacter {
                id: value.to_owned(),
                found: first,
            });
        }
        if let Some(found) = segment
            .chars()
            .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_'))
        {
            return Err(IdError::InvalidCharacter {
                id: value.to_owned(),
                found,
            });
        }
    }
    Ok(())
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(Arc<str>);

        impl $name {
            /// Validates and wraps `value`.
            pub fn new(value: &str) -> Result<Self, IdError> {
                validate(value)?;
                Ok(Self(Arc::from(value)))
            }

            /// Wraps a literal written in source code.
            ///
            /// # Panics
            /// Panics when the literal is not a valid identifier. Use [`Self::new`]
            /// for anything read from a file, a process or the user.
            #[track_caller]
            pub fn literal(value: &'static str) -> Self {
                match Self::new(value) {
                    Ok(id) => id,
                    Err(error) => panic!("invalid {} literal: {error}", stringify!($name)),
                }
            }

            /// The validated text, as written in manifests and element ids.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;
            fn from_str(value: &str) -> Result<Self, IdError> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;
            fn try_from(value: String) -> Result<Self, IdError> {
                Self::new(&value)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0.as_ref().to_owned()
            }
        }
    };
}

id_type!(
    /// Identity of a module: bundled Rust module, declarative package or process plugin.
    ModuleId
);
id_type!(
    /// Identity of a command. Menus, toolbars and key bindings refer to commands by this id.
    CommandId
);
id_type!(
    /// Identity of a docked view (left or bottom area).
    ViewId
);
id_type!(
    /// Identity of a top-level menu.
    MenuId
);
id_type!(
    /// Kind of document a module can open in the centre area, e.g. `logs.source`.
    DocumentKind
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_dotted_lowercase_identifiers() {
        for valid in ["a", "retrowork.logs", "logs.filter-2.set_all", "0day.x"] {
            assert_eq!(ModuleId::new(valid).unwrap().as_str(), valid);
        }
    }

    #[test]
    fn rejects_malformed_identifiers_with_a_specific_reason() {
        assert_eq!(CommandId::new(""), Err(IdError::Empty));
        assert_eq!(
            CommandId::new("a..b"),
            Err(IdError::EmptySegment("a..b".into()))
        );
        assert_eq!(
            CommandId::new(".a"),
            Err(IdError::EmptySegment(".a".into()))
        );
        assert_eq!(
            CommandId::new("a.-b"),
            Err(IdError::InvalidSegmentStart("a.-b".into()))
        );
        assert_eq!(
            CommandId::new("Logs.open"),
            Err(IdError::InvalidCharacter {
                id: "Logs.open".into(),
                found: 'L'
            })
        );
        assert_eq!(
            CommandId::new("logs open"),
            Err(IdError::InvalidCharacter {
                id: "logs open".into(),
                found: ' '
            })
        );
        let long = "a".repeat(MAX_ID_LEN + 1);
        assert_eq!(CommandId::new(&long), Err(IdError::TooLong(long.clone())));
        assert!(CommandId::new(&long[..MAX_ID_LEN]).is_ok());
    }

    #[test]
    fn deserialization_validates() {
        let ok: ViewId = serde_json::from_str("\"logs.sources\"").unwrap();
        assert_eq!(ok.as_str(), "logs.sources");
        assert!(serde_json::from_str::<ViewId>("\"Bad Id\"").is_err());
        assert_eq!(serde_json::to_string(&ok).unwrap(), "\"logs.sources\"");
    }
}
