// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_io as fio;
use flyweights::FlyStr;
use static_assertions::assert_eq_size;
use std::{
    borrow::Borrow,
    ffi::{CString, IntoStringError},
    fmt,
};
use thiserror::Error;

/// The maximum length, in bytes, of a namespace path.
///
/// It is defined to be the same as [`fio::MAX_PATH_LENGTH`] to reduce the distinction
/// between namespaces and remote filesystems.
pub const MAX_PATH_LENGTH: usize = fio::MAX_PATH_LENGTH as usize;
assert_eq_size!(u64, usize);

/// [Path] represents the path of a Zircon process namespace entry.
///
/// A [Path] is:
///
/// - Never empty
/// - Always start with "/"
/// - Contains no empty path segments
/// - Valid UTF-8
/// - Never longer than [`MAX_PATH_LENGTH`]
/// - Each segment must be a valid [`name::Name`], meaning:
///   * It cannot be longer than [`fio::MAX_NAME_LENGTH`].
///   * It cannot be empty.
///   * It cannot be ".." (dot-dot).
///   * It cannot be "." (single dot).
///   * It cannot contain "/".
///   * It cannot contain embedded NUL.
///
/// [Path] is both a valid C string, and a valid UTF-8 string. It takes on both
/// constraints because namespace paths are sent via FIDL types which use UTF-8 [String]s,
/// and eventually passed to the Zircon process ABI which require C strings (i.e. no
/// embedded NULs).
#[derive(Eq, Ord, PartialOrd, PartialEq, Debug, Hash, Clone)]
pub struct Path {
    path: FlyStr,

    /// Index of the last slash.
    dirname_idx: usize,
}

#[derive(Debug, Clone, Error)]
pub enum PathError {
    #[error("path cannot be empty")]
    Empty,

    #[error("path `{0}` must not be longer than fuchsia.io/MAX_PATH_LENGTH")]
    TooLong(String),

    #[error("path is not UTF8: `{0}`")]
    NotUtf8(#[from] IntoStringError),

    #[error("path `{0}` must start with a slash")]
    NoLeadingSlash(String),

    #[error("path has an invalid path segment: {0}")]
    InvalidSegment(#[from] name::ParseNameError),
}

impl Path {
    pub fn new(path: impl AsRef<str> + Into<String>) -> Result<Self, PathError> {
        let str: &str = path.as_ref();
        if str.is_empty() {
            return Err(PathError::Empty);
        }
        if str.len() > MAX_PATH_LENGTH {
            return Err(PathError::TooLong(str.to_owned()));
        }
        if !str.starts_with('/') {
            return Err(PathError::NoLeadingSlash(str.to_owned()));
        }
        // A single slash is always valid, but splitting a slash using a slash
        // would produce an empty substring.
        if str != "/" {
            for segment in str[1..].split('/') {
                name::validate_name(segment)?;
            }
        }
        let str: &str = path.as_ref();
        let dirname_idx = str.rfind('/').expect("path validation is wrong");
        let path = FlyStr::new(path);
        Ok(Self { path, dirname_idx })
    }

    /// Splits the path according to "/".
    pub fn split(&self) -> Vec<String> {
        self.to_string()
            .split("/")
            // `split("/")` produces empty segments if there is nothing before or after a slash.
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    }

    /// Path to the containing directory, or "/" if the path is "/".
    pub fn dirname(&self) -> &str {
        if self.dirname_idx == 0 {
            "/"
        } else {
            &self.path[0..self.dirname_idx]
        }
    }

    /// Path to the containing directory, or "/" if the path is "/", as a [`Path`].
    pub fn dirname_ns_path(&self) -> Self {
        // Unwrapping is safe because the original path has been validated so that means the
        // dirname substring should still be a valid path.
        Self::new(self.dirname()).unwrap()
    }

    /// The remainder of the path after `dirname`.
    pub fn basename(&self) -> &str {
        &self.path[self.dirname_idx + 1..]
    }

    pub fn as_str(&self) -> &str {
        self.as_ref()
    }
}

impl serde::ser::Serialize for Path {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.path.serialize(serializer)
    }
}

impl TryFrom<CString> for Path {
    type Error = PathError;

    fn try_from(path: CString) -> Result<Self, PathError> {
        Path::new(path.into_string()?)
    }
}

impl From<Path> for CString {
    fn from(path: Path) -> Self {
        // SAFETY: in `Path::new` we already verified that there are no
        // embedded NULs.
        unsafe { CString::from_vec_unchecked(path.path.as_bytes().to_owned()) }
    }
}

impl TryFrom<String> for Path {
    type Error = PathError;

    fn try_from(path: String) -> Result<Self, PathError> {
        Path::new(path)
    }
}

impl From<Path> for String {
    fn from(path: Path) -> Self {
        path.path.into()
    }
}

impl AsRef<str> for Path {
    fn as_ref(&self) -> &str {
        &self.path
    }
}

impl Borrow<str> for Path {
    fn borrow(&self) -> &str {
        &self.path
    }
}

impl TryFrom<&'_ str> for Path {
    type Error = PathError;

    fn try_from(value: &'_ str) -> Result<Self, Self::Error> {
        Path::new(value)
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let str: &str = self.as_ref();
        str.fmt(f)
    }
}

/// Make a [`NamespacePath`] from a string in tests or crash if invalid.
#[cfg(test)]
pub fn ns_path(str: &str) -> Path {
    Path::new(str).unwrap()
}

#[cfg(test)]
mod test {
    use super::*;

    fn path(path: &str) -> Path {
        Path::new(path.to_owned()).unwrap()
    }

    #[test]
    fn test_embedded_nul() {
        assert!(Path::new("/foo/bar").is_ok());
        assert!(Path::new("/foo\0b/bar").is_err());
    }

    #[test]
    fn test_split() {
        assert_eq!(path("/").split(), Vec::<String>::new());
        assert_eq!(path("/foo").split(), vec!["foo".to_owned()]);
        assert_eq!(path("/foo/bar").split(), vec!["foo".to_owned(), "bar".to_owned()]);
    }

    #[test]
    fn test_dirname() {
        assert_eq!(path("/").dirname(), "/");
        assert_eq!(path("/foo").dirname(), "/");
        assert_eq!(path("/foo/bar").dirname(), "/foo");
    }

    #[test]
    fn test_basename() {
        assert_eq!(path("/").basename(), "");
        assert_eq!(path("/foo").basename(), "foo");
        assert_eq!(path("/foo/bar").basename(), "bar");
    }
}
