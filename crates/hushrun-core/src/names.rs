//! Names of scopes, stages, checks and artifacts (L-2).
//!
//! Every name matches `^[a-z][a-z0-9_-]{0,62}$`. Where a name appears in an environment
//! variable or a dotenv key, it is uppercased and `-` becomes `_` ([`env_key`]).

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

/// The longest name allowed, in bytes.
pub const MAX_NAME_LEN: usize = 63;

/// Why a string is not a valid name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind} name {name:?} is invalid: names match ^[a-z][a-z0-9_-]{{0,62}}$")]
pub struct NameError {
    pub kind: &'static str,
    pub name: String,
}

/// Checks a name against `^[a-z][a-z0-9_-]{0,62}$`.
pub fn is_valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    match bytes.first() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    bytes.len() <= MAX_NAME_LEN
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// A name as it appears in environment variables and dotenv keys: uppercased, with `-`
/// replaced by `_`.
pub fn env_key(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '-' {
                '_'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect()
}

macro_rules! name_type {
    ($(#[$doc:meta])* $ty:ident, $kind:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $ty(String);

        impl $ty {
            /// What kind of name this is, for messages.
            pub const KIND: &'static str = $kind;

            /// Validates `name`.
            pub fn new(name: impl Into<String>) -> Result<Self, NameError> {
                let name = name.into();
                if is_valid_name(&name) {
                    Ok(Self(name))
                } else {
                    Err(NameError { kind: $kind, name })
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// The name in environment variables and dotenv keys.
            pub fn env_key(&self) -> String {
                env_key(&self.0)
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $ty {
            type Err = NameError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $ty {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl PartialEq<str> for $ty {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $ty {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let name = String::deserialize(deserializer)?;
                Self::new(name).map_err(serde::de::Error::custom)
            }
        }
    };
}

name_type!(
    /// The name of a scope: a deployable part of the repository.
    ScopeName,
    "scope"
);
name_type!(
    /// The name of a stage, such as `staging` or `production`.
    StageName,
    "stage"
);
name_type!(
    /// The name of a check, such as `unit` or `e2e`.
    CheckName,
    "check"
);
name_type!(
    /// The name of an artifact, such as `image`.
    ArtifactName,
    "artifact"
);

impl ScopeName {
    /// The scope of a repository whose policy declares none.
    pub fn default_scope() -> Self {
        Self(crate::DEFAULT_SCOPE.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: L-2
    #[test]
    fn validates_names() {
        for good in ["a", "default", "e2e", "api-v2", "my_scope", &"a".repeat(63)] {
            assert!(is_valid_name(good), "{good}");
        }
        for bad in [
            "",
            "A",
            "1a",
            "-a",
            "_a",
            "a.b",
            "a b",
            "é",
            &"a".repeat(64),
        ] {
            assert!(!is_valid_name(bad), "{bad}");
        }
    }

    // covers: L-2
    #[test]
    fn maps_names_to_env_keys() {
        assert_eq!(env_key("api-v2"), "API_V2");
        assert_eq!(env_key("my_scope"), "MY_SCOPE");
        assert_eq!(CheckName::new("e2e").unwrap().env_key(), "E2E");
    }

    #[test]
    fn deserializes_with_validation() {
        let name: ScopeName = serde_json::from_str("\"web\"").unwrap();
        assert_eq!(name, "web");
        let err = serde_json::from_str::<ScopeName>("\"Web\"").unwrap_err();
        assert!(
            err.to_string().contains("scope name \"Web\" is invalid"),
            "{err}"
        );
    }
}
