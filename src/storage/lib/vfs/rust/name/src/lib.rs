// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Data structures and functions relevant to `fuchsia.io` name processing.
//!
//! These names may be used to designate the location of a node as it
//! appears in a directory.
//!
//! These should be aligned with the library comments in sdk/fidl/fuchsia.io/io.fidl.

use {
    fidl_fuchsia_io as fio, fuchsia_zircon_status as zx, static_assertions::const_assert_eq,
    std::borrow::Borrow, std::fmt::Display, std::ops::Deref, thiserror::Error,
};

/// The type for the name of a node, i.e. a single path component, e.g. `foo`.
///
/// ## Invariants
///
/// A valid node name must meet the following criteria:
///
/// * It cannot be longer than [MAX_NAME_LENGTH].
/// * It cannot be empty.
/// * It cannot be ".." (dot-dot).
/// * It cannot be "." (single dot).
/// * It cannot contain "/".
/// * It cannot contain embedded NUL.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Name(String);

/// The maximum length, in bytes, of a single filesystem component.
pub const MAX_NAME_LENGTH: usize = fio::MAX_NAME_LENGTH as usize;
const_assert_eq!(MAX_NAME_LENGTH as u64, fio::MAX_NAME_LENGTH);

#[derive(Error, Debug, Clone)]
pub enum ParseNameError {
    #[error("name `{0}` is too long")]
    TooLong(String),

    #[error("name cannot be empty")]
    Empty,

    #[error("name cannot be `.`")]
    Dot,

    #[error("name cannot be `..`")]
    DotDot,

    #[error("name cannot contain `/`")]
    Slash,

    #[error("name cannot contain embedded NUL")]
    EmbeddedNul,
}

impl From<ParseNameError> for zx::Status {
    fn from(value: ParseNameError) -> Self {
        match value {
            ParseNameError::TooLong(_) => zx::Status::BAD_PATH,
            _ => zx::Status::INVALID_ARGS,
        }
    }
}

impl Name {
    pub fn from<S: Into<String>>(s: S) -> Result<Name, ParseNameError> {
        parse_name(s.into())
    }
}

impl Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = &self.0;
        write!(f, "{value}")
    }
}

/// Parses a string name into a [Name].
pub fn parse_name(name: String) -> Result<Name, ParseNameError> {
    validate_name(&name)?;
    Ok(Name(name))
}

/// Check whether a string name will be a valid input to [Name].
pub fn validate_name(name: &str) -> Result<(), ParseNameError> {
    if name.len() > MAX_NAME_LENGTH {
        return Err(ParseNameError::TooLong(name.to_string()));
    }
    if name.len() == 0 {
        return Err(ParseNameError::Empty);
    }
    if name == "." {
        return Err(ParseNameError::Dot);
    }
    if name == ".." {
        return Err(ParseNameError::DotDot);
    }
    if name.chars().any(|c: char| c == '/') {
        return Err(ParseNameError::Slash);
    }
    if name.chars().any(|c: char| c == '\0') {
        return Err(ParseNameError::EmbeddedNul);
    }
    Ok(())
}

impl From<Name> for String {
    fn from(value: Name) -> Self {
        value.0
    }
}

impl TryFrom<String> for Name {
    type Error = ParseNameError;

    fn try_from(value: String) -> Result<Name, ParseNameError> {
        parse_name(value)
    }
}

impl Deref for Name {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Borrow<str> for Name {
    fn borrow(&self) -> &str {
        &*self
    }
}

#[cfg(test)]
mod tests {
    use {super::*, assert_matches::assert_matches};

    #[test]
    fn test_parse_name() {
        assert_matches!(parse_name("a".repeat(1000)), Err(ParseNameError::TooLong(_)));
        assert_matches!(
            parse_name(
                std::str::from_utf8(&vec![65; fio::MAX_FILENAME as usize + 1]).unwrap().to_string()
            ),
            Err(ParseNameError::TooLong(_))
        );
        assert_matches!(
            parse_name(
                std::str::from_utf8(&vec![65; fio::MAX_FILENAME as usize]).unwrap().to_string()
            ),
            Ok(_)
        );
        assert_matches!(parse_name("".to_string()), Err(ParseNameError::Empty));
        assert_matches!(parse_name(".".to_string()), Err(ParseNameError::Dot));
        assert_matches!(parse_name("..".to_string()), Err(ParseNameError::DotDot));
        assert_matches!(parse_name(".a".to_string()), Ok(Name(name)) if &name == ".a");
        assert_matches!(parse_name("..a".to_string()), Ok(Name(name)) if &name == "..a");
        assert_matches!(parse_name("a/b".to_string()), Err(ParseNameError::Slash));
        assert_matches!(parse_name("a\0b".to_string()), Err(ParseNameError::EmbeddedNul));
        assert_matches!(parse_name("abc".to_string()), Ok(Name(name)) if &name == "abc");
    }

    #[test]
    fn test_validate_name() {
        assert_matches!(validate_name(&"a".repeat(1000)), Err(ParseNameError::TooLong(_)));
        assert_matches!(
            validate_name(std::str::from_utf8(&vec![65; fio::MAX_FILENAME as usize + 1]).unwrap()),
            Err(ParseNameError::TooLong(_))
        );
        assert_matches!(
            validate_name(std::str::from_utf8(&vec![65; fio::MAX_FILENAME as usize]).unwrap()),
            Ok(())
        );
        assert_matches!(validate_name(""), Err(ParseNameError::Empty));
        assert_matches!(validate_name("."), Err(ParseNameError::Dot));
        assert_matches!(validate_name(".."), Err(ParseNameError::DotDot));
        assert_matches!(validate_name(".a"), Ok(()));
        assert_matches!(validate_name("..a"), Ok(()));
        assert_matches!(validate_name("a/b"), Err(ParseNameError::Slash));
        assert_matches!(validate_name("a\0b"), Err(ParseNameError::EmbeddedNul));
        assert_matches!(validate_name("abc"), Ok(()));
    }

    #[test]
    fn test_try_from() {
        assert_matches!(Name::try_from("a".repeat(1000)), Err(ParseNameError::TooLong(_)));
        assert_matches!(Name::try_from("abc".to_string()), Ok(Name(name)) if &name == "abc");
    }

    #[test]
    fn test_into() {
        let name = Name::try_from("a".to_string()).unwrap();
        let name: String = name.into();
        assert_eq!(name, "a".to_string());
    }

    #[test]
    fn test_deref() {
        let name = Name::try_from("a".to_string()).unwrap();
        let name: &str = &name;
        assert_eq!(name, "a");
    }
}
