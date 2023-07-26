// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use std::{
    hash::Hash,
    marker::PhantomData,
    path::{Path, PathBuf},
};

/// A base trait for TypePath's marker traits.
pub trait PathTypeMarker {
    /// A reference to an object that implements Display, and gives the
    /// displayable semantic type for this path.  This is used by the Debug
    /// implementation of `TypedPathBuf` to display the semantic type for the
    /// path:
    ///
    /// ```
    /// struct MarkerStructType;
    /// impl_path_type_marker!(MarkerStructType);
    ///
    /// let typed_path = TypedPathBuf<MarkerStructType>::from("some/path");
    /// println!("{:?}", typed_path);
    /// ```
    /// will print:
    ///
    /// ```text
    /// TypedPathBuf<MarkerStructType>("some/path")
    /// ```
    fn path_type_display() -> &'static dyn std::fmt::Display;
}

/// Implement the `PathTypeMarker` trait for a given marker-type struct.  This
/// mainly simplifies the creation of a display-string for the type.
#[macro_export]
macro_rules! impl_path_type_marker {
    // This macro takes an argument of the marker struct's type name, and then
    // provides an implementation of 'PathTypeMarker' for it.
    ($struct_name:ident) => {
        impl PathTypeMarker for $struct_name {
            fn path_type_display() -> &'static dyn std::fmt::Display {
                &stringify!($struct_name)
            }
        }
    };
}

/// A path, in valid utf-8, which carries a marker for what kind of path it is.
#[derive(Clone, Serialize, Deserialize)]
#[repr(transparent)]
#[serde(transparent)]
pub struct TypedPathBuf<P: PathTypeMarker> {
    #[serde(flatten)]
    inner: Utf8PathBuf,

    #[serde(skip)]
    _marker: PhantomData<P>,
}

/// This derefs into the typed version of utf8 path, not utf8 path itself, so
/// that it is easier to use in typed contexts, and makes the switchover to
/// a non-typed context more explicit.
///
/// This also causes any path manipulations (join, etc.) to be done without the
/// semantic type, so that the caller has to be explicit that it's still the
/// semantic type (using 'into()', for instance).
impl<P: PathTypeMarker> std::ops::Deref for TypedPathBuf<P> {
    type Target = Utf8PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<P: PathTypeMarker> TypedPathBuf<P> {
    /// Convert this TypedPathBuf into a standard (OsStr-based) `PathBuf`.  This
    /// both strips it of semantic type and that it's known to be Utf-8.
    pub fn into_std_path_buf(self) -> PathBuf {
        self.inner.into_std_path_buf()
    }
}

impl<P: PathTypeMarker> AsRef<Utf8Path> for TypedPathBuf<P> {
    fn as_ref(&self) -> &Utf8Path {
        self.inner.as_ref()
    }
}

impl<P: PathTypeMarker> AsRef<Path> for TypedPathBuf<P> {
    fn as_ref(&self) -> &Path {
        self.inner.as_ref()
    }
}

/// The Debug implementation displays like a type-struct that carries the marker
/// type for the path:
///
/// ```text
/// TypedPathBuf<MarkerStructType>("some/path")
/// ```
impl<P: PathTypeMarker> std::fmt::Debug for TypedPathBuf<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple(&format!("TypedPathBuf<{}>", P::path_type_display()))
            .field(&self.inner.to_string())
            .finish()
    }
}

/// The Display implementation defers to the wrapped path.
impl<P: PathTypeMarker> std::fmt::Display for TypedPathBuf<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

/// Implement From<> for path-like sources.  Note that these also will infer the
/// semantic type, which while useful in some contexts, can cause issues in
/// places where multiple different type markers are used:
///
/// ```
/// fn some_func(source: TypedPathBuf<Source>,     TypedPathBuf<Destination>);
///
/// // This infers the types of the paths:
/// some_func("source_path".into(), "destination_path".into());
///
/// // allowing this error:
/// some_func("destination_path".into(), "source_path",into());
///
/// // In these cases, it's best to strongly type one or both of them:
/// some_func(TypedPathBuf<Source>::from("source_path"), "destination_path".into());
///
/// // or (better)
/// some_func(TypedPathBuf<Source>::from("source_path"),
///           TypedPathBuf<Destination>::from("destination_path"));
/// ```
// inner module used to group impls and to add above documentation.
mod from_impls {
    use super::*;

    impl<P: PathTypeMarker> From<Utf8PathBuf> for TypedPathBuf<P> {
        fn from(path: Utf8PathBuf) -> Self {
            Self { inner: path, _marker: PhantomData }
        }
    }

    impl<P: PathTypeMarker> From<TypedPathBuf<P>> for Utf8PathBuf {
        fn from(path: TypedPathBuf<P>) -> Self {
            path.inner
        }
    }

    impl<P: PathTypeMarker> From<TypedPathBuf<P>> for PathBuf {
        fn from(path: TypedPathBuf<P>) -> Self {
            path.inner.into()
        }
    }

    impl<P: PathTypeMarker> From<String> for TypedPathBuf<P> {
        fn from(s: String) -> TypedPathBuf<P> {
            TypedPathBuf::from(Utf8PathBuf::from(s))
        }
    }

    impl<P: PathTypeMarker> From<&str> for TypedPathBuf<P> {
        fn from(s: &str) -> TypedPathBuf<P> {
            TypedPathBuf::from(Utf8PathBuf::from(s))
        }
    }

    impl<P: PathTypeMarker> std::str::FromStr for TypedPathBuf<P> {
        type Err = String;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            Ok(Self::from(s))
        }
    }
}

// These comparison implementations are required because #[derive(...)] will not
// derive these if `P` doesn't implement them, but `P` has no reason to
// implement them, so these implementations just pass through to the Utf8PathBuf
// implementations.

impl<P: PathTypeMarker> PartialOrd for TypedPathBuf<P> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<P: PathTypeMarker> Ord for TypedPathBuf<P> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.inner.cmp(&other.inner)
    }
}

impl<P: PathTypeMarker> PartialEq for TypedPathBuf<P> {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

impl<P: PathTypeMarker> Eq for TypedPathBuf<P> {}

impl<P: PathTypeMarker> Hash for TypedPathBuf<P> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.inner.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    struct TestPathType {}
    impl_path_type_marker!(TestPathType);

    #[test]
    fn make_typed_path_from_string() {
        let original: String = "/this/is/a/string".to_string();
        let typed = TypedPathBuf::<TestPathType>::from_str(&original).unwrap();
        assert_eq!(typed.to_string(), original);
    }

    #[test]
    fn make_typed_path_from_str() {
        let original: &str = "/this/is/a/string";
        let typed = TypedPathBuf::<TestPathType>::from_str(&original).unwrap();
        assert_eq!(typed.to_string(), original);
    }

    #[test]
    fn path_type_deserialization() {
        #[derive(Debug, Deserialize)]
        struct Sample {
            pub path: TypedPathBuf<TestPathType>,
        }
        let parsed: Sample = serde_json::from_str("{ \"path\": \"this/is/a/path\"}").unwrap();
        assert_eq!(parsed.path, TypedPathBuf::<TestPathType>::from("this/is/a/path"));
    }

    #[test]
    fn path_type_serialization() {
        #[derive(Debug, Serialize)]
        struct Sample {
            pub path: TypedPathBuf<TestPathType>,
        }
        let sample = Sample { path: "this/is/a/path".into() };
        let expected = serde_json::json!({ "path": "this/is/a/path"});
        assert_eq!(serde_json::to_value(sample).unwrap(), expected);
    }

    #[test]
    fn typed_path_debug_impl() {
        let typed = TypedPathBuf::<TestPathType>::from("some/path");
        assert_eq!(format!("{:?}", typed), "TypedPathBuf<TestPathType>(\"some/path\")");
    }

    #[test]
    fn typed_path_display_impl() {
        let typed = TypedPathBuf::<TestPathType>::from("some/path");
        assert_eq!(format!("{}", typed), "some/path");
    }

    #[test]
    fn typed_path_buf_into_path_buf() {
        let typed = TypedPathBuf::<TestPathType>::from("some/path");
        assert_eq!(typed.into_std_path_buf(), Utf8PathBuf::from("some/path"));
    }

    #[test]
    fn typed_path_derefs_into_utf8_path() {
        let typed = TypedPathBuf::<TestPathType>::from("some/path");
        let utf8_path = Utf8PathBuf::from("some/path");
        assert_eq!(*typed, utf8_path);
    }

    #[test]
    fn typed_path_as_ref_utf8path() {
        let original = TypedPathBuf::<TestPathType>::from("a/path");
        let path: &Utf8Path = original.as_ref();
        assert_eq!(path, Utf8Path::new("a/path"))
    }

    #[test]
    fn typed_path_as_ref_path() {
        let original = TypedPathBuf::<TestPathType>::from("a/path");
        let path: &Path = original.as_ref();
        assert_eq!(path, Path::new("a/path"))
    }
}
