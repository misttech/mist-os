// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A FileRelativePathBuf is a path which is stored in a file as being relative
//! to the file it's stored in.
//!
//! When a file contains paths to other files, those paths either need to be
//! absolute, or need to relative to some base.  Consider the following tree of
//! directories and files:
//!
//! ```
//! <root>/
//!  +-- resources/
//!      +-- foo/
//!      |    +-- file_1.json
//!      +-- bar/
//!      |    +-- file_2.json
//!      |    +-- file_3.json
//!      +-- manifest.json
//! ```
//!
//! The file `manifest.json` contains the paths to each of the json files.
//!
//! If those paths are relative to itself (ie, file-relative), then they would
//! be:
//!
//! - foo/file_1.json
//! - bar/file_2.json
//! - bar/file_3.json
//!
//! However, for a program running in the root folder, the path to the
//! `manifest.json` file is `resources/manifest.json`, and the paths to the json
//! files it lists, when "resolved" against the path to the `manifest.json` file
//! are:
//!
//! - resources/foo/file_1.json
//! - resources/bar/file_2.json
//! - resources/bar/file_3.json
//!
//! This crate is meant to simplify the parsing and use of these files by
//! performing most of the path resolution and relativization.
//!
//! This crate offers two pieces that work in concert, an enum for the paths in
//! the file (to track their file-relative or resolved state), and a trait which
//! can be easily implemented by structs that contain these paths to handle the
//! conversion of the structure from one form to another.
//!

use anyhow::{anyhow, Result};
use assembly_container::{FileType, WalkPaths, WalkPathsFn};
use camino::{Utf8Path, Utf8PathBuf};
use pathdiff::diff_utf8_paths;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// A Utf8PathBuf which can be either file-relative, or has been resolved with
/// the path to the containing file.
///
/// The 'serde::Deserialize' implementation results in these path being in the
/// file-relative state.
///
/// The FileRelativePathBuf has 'From' implementations that create it in the
/// "resolved" state when converting from path formats that are used by an
/// application (str, String, Utf8Path, Utf8PathBuf, etc.)
///
/// ```
/// use assembly_file_relative_path::FileRelativePathBuf;
///
/// let path: FileRelativePathBuf = "some/path/to/file_1.json".into();
/// assert_eq!(
///   path,
///   FileRelativePathBuf::Resolved("some/path/to/file_1.json".into())
/// );
///
/// let relative = path.make_relative_to_dir("some/path")?;
/// assert_eq!(
///   relative,
///   FileRelativePathBuf::FileRelative("to/file_1.json".into())
/// );
/// ```
///
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, JsonSchema)]
#[serde(from = "FileRelativePathBufSerializationHelper")]
#[serde(into = "FileRelativePathBufSerializationHelper")]
pub enum FileRelativePathBuf {
    /// The path is file-relative.
    ///
    /// For structs that are stored in files that contain to other paths, which
    /// are relative to the containing file, this is the serialized form of this
    /// type.
    ///
    /// When first deserialized file a file (or otherwise using
    /// 'serde::Deserialize`, this is the state that the FileRelativePathBuf
    /// will be in.
    ///
    /// From this state, it can can be resolved against the path to the
    /// containing file to make it usable by the application.
    #[schemars(schema_with = "path_schema")]
    FileRelative(Utf8PathBuf),

    /// The path has been 'resolved'.  This is the state it's in when created directly
    /// using From/Into.  This state can be made relative from a path to a file
    /// that will contain this path as a file-relative path.
    #[schemars(schema_with = "path_schema")]
    Resolved(Utf8PathBuf),
}

impl WalkPaths for FileRelativePathBuf {
    fn walk_paths_with_dest<F: WalkPathsFn>(
        &mut self,
        found: &mut F,
        dest: Utf8PathBuf,
    ) -> Result<()> {
        match self {
            FileRelativePathBuf::FileRelative(path_in_file) => {
                found(path_in_file, dest, FileType::Unknown)
            }
            FileRelativePathBuf::Resolved(resolved_path) => {
                found(resolved_path, dest, FileType::Unknown)
            }
        }
    }
}

fn path_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    let mut schema: schemars::schema::SchemaObject = <String>::json_schema(gen).into();
    schema.format = Some("Utf8PathBuf".to_owned());
    schema.into()
}

impl FileRelativePathBuf {
    /// Convert the file-relative path into a resolved path that combines the
    /// 'path_to_dir' and the file-relative path together.
    ///
    /// NOTE: This version takes a path to a _directory_, while the
    /// `resolve_path_from_file()` fn takes a path to a _file_.
    ///
    /// ```
    /// let path = FileRelativePathBuf::FileRelative("foo/file_1.json".into());
    /// let resolved_path = path.resolve_from_dir("resources".into());
    ///
    /// assert_eq!(
    ///   resolved_path,
    ///   Some(FileRelativePathBuf::Resolved("resources/foo/file_1.json"))
    /// );
    ///```
    pub fn resolve_from_dir(self, path_to_dir: impl AsRef<Utf8Path>) -> Result<Self> {
        match self {
            FileRelativePathBuf::FileRelative(file_relative) => {
                Ok(Self::Resolved(path_to_dir.as_ref().join(file_relative)))
            }
            passthrough @ FileRelativePathBuf::Resolved(..) => Ok(passthrough),
        }
    }

    /// Take a resolved path, and make it relative to a file.
    ///
    /// ```
    /// let resolved_path = FileRelativePathBuf::Resolved("resources/bar/file_2.json".into());
    /// let file_relative = resolved_path.make_relative_to_file("resources/manifest.json".into());
    ///
    /// assert_eq!(
    ///   file_relative,
    ///   Some(FileRelativePathBuf::FileRelative("bar/file_2.json"))
    /// );
    /// ```
    pub fn make_relative_to_file(self, path_to_file: impl AsRef<Utf8Path>) -> Result<Self> {
        match self {
            resolved @ Self::Resolved(..) => {
                let dir = get_file_parent_dir(path_to_file.as_ref())?;
                resolved.make_relative_to_dir(dir)
            }
            passthrough @ Self::FileRelative(..) => Ok(passthrough),
        }
    }

    /// Take a resolved path, and make it relative to a directory.
    ///
    /// ```
    /// let resolved_path = FileRelativePathBuf::Resolved("resources/bar/file_2.json".into());
    /// let file_relative = resolved_path.make_relative_to_dir("resources");
    ///
    /// assert_eq!(
    ///   file_relative,
    ///   Some(FileRelativePathBuf::FileRelative("bar/file_2.json"))
    /// );
    /// ```
    pub fn make_relative_to_dir(self, dir: impl AsRef<Utf8Path>) -> Result<Self> {
        match self {
            Self::Resolved(resolved) => {
                let file_relative = diff_utf8_paths(&resolved, &dir).ok_or_else(|| {
                    anyhow!(
                        "Unable to make a path to '{}' that's relative to '{}'.",
                        resolved,
                        dir.as_ref()
                    )
                })?;
                Ok(Self::FileRelative(file_relative))
            }
            passthrough @ Self::FileRelative { .. } => Ok(passthrough),
        }
    }

    /// Use this FileRelativePathBuf as a simple Utf8PathBuf
    pub fn as_utf8_pathbuf(&self) -> &Utf8PathBuf {
        match self {
            FileRelativePathBuf::FileRelative(path_in_file) => path_in_file,
            FileRelativePathBuf::Resolved(resolved_path) => resolved_path,
        }
    }

    /// Use this FileRelativePathBuf as a simple mutable Utf8PathBuf
    pub fn as_mut_utf8_pathbuf(&mut self) -> &mut Utf8PathBuf {
        match self {
            FileRelativePathBuf::FileRelative(path_in_file) => path_in_file,
            FileRelativePathBuf::Resolved(resolved_path) => resolved_path,
        }
    }

    /// Convert this FileRelativePathBuf to a simple Utf8PathBuf
    pub fn to_utf8_pathbuf(self) -> Utf8PathBuf {
        match self {
            FileRelativePathBuf::FileRelative(path_in_file) => path_in_file,
            FileRelativePathBuf::Resolved(resolved_path) => resolved_path,
        }
    }
}

/// This trait can be implemented by structs to allow them to easily resolve or
/// make file-relative all of their FileRelativePathBuf fields.
///
/// The implementation of this trait is straightforward, and can be moved to a
/// derive macro in the future.
///
/// ```
/// struct Foo {
///   some_relative_path: FileRelativePathBuf,
///   some_flag: bool,
///   child: Bar,  // also impl's SupportsFileRelativePaths
/// }
///
/// struct Bar {
///   some_other_path: FileRelativePathBuf
/// }
///
/// impl SupportsFileRelativePaths for Foo {
///
///   fn resolve_paths_from_dir(self, path_to_dir: impl AsRef<Utf8Path>) -> Result<Self> {
///     let path_to_file = path_to_file.as_ref();
///     Ok( Self {
///       some_relative_path: self.some_relative_path.resolve_path_from_dir(&path_to_dir)?,
///       child: self.child.resolve_paths_from_dir(&path_to_dir)?,
///       ..self
///     })
///   }
/// }
///
/// let foo = Foo{ ... }
/// let resolved = foo.resolve_paths_from_file("path/to/file.txt")?;
///
/// ```
pub trait SupportsFileRelativePaths {
    /// After deserializing a file containing file-relative paths, this function
    /// is used to convert all the FileRelativePathBufs into their resolved form.
    ///
    /// This is used after deserializing the struct from a file, with the path
    /// to that file.
    ///
    /// ```
    /// struct Foo {
    ///   some_path: FileRelativePathBuf,
    /// }
    /// // trait impl omitted for brevity.
    ///
    /// let foo = Foo {
    ///   some_path: FileRelativePathBuf::FileRelative("some/file.json".into())
    /// };
    /// let foo_resolved = foo.resolve_paths_from_file("path/to/containing_file.json")?;
    ///
    /// assert_eq(
    ///     foo_resolved.some_path,
    ///     FileRelativePathBuf::Resolved("path/to/some/file.json".into())
    /// );
    /// ```
    fn resolve_paths_from_file(self, path_to_containing_file: impl AsRef<Utf8Path>) -> Result<Self>
    where
        Self: Sized,
    {
        let dir = get_file_parent_dir(path_to_containing_file.as_ref())?;
        self.resolve_paths_from_dir(&dir)
    }

    /// Convert all the FileRelativePathBufs in this struct (recursively) into
    /// the Resolved state, using the path to the directory of the file that
    /// contains file-relative paths.
    ///
    /// This is the function that needs to be implemented by structs.
    ///
    /// ```
    /// struct Foo {
    ///   some_path: FileRelativePathBuf,
    /// }
    /// // trait impl omitted for brevity.
    ///
    /// let foo = Foo {
    ///     some_path: FileRelativePathBuf::FileRelative("some/file.json".into())
    /// };
    /// let foo_resolved = foo.resolve_paths_from_dir("path/to".into())?;
    ///
    /// assert_eq(
    ///   foo_resolved.some_path,
    ///   FileRelativePathBuf::Resolved("path/to/some/file.json".into())
    /// );
    /// ```
    fn resolve_paths_from_dir(self, dir_path: impl AsRef<Utf8Path>) -> Result<Self>
    where
        Self: Sized;

    /// Convert all the FileRelativePathBufs in this struct (recursively) into
    /// the FileRelative state, using the path to the file that will contain the
    /// file-relative paths.
    ///
    /// This is used before serializing the struct to a file, with the path to
    /// that file.
    ///
    /// ```
    /// struct Foo {
    ///   some_path: FileRelativePathBuf,
    /// }
    /// // trait impl omitted for brevity.
    ///
    /// let foo = Foo {
    ///   some_path: FileRelativePathBuf::FileRelative("some/file.json".into())
    /// };
    /// let foo_relative = foo.make_paths_relative_to_file("path/to/containing_file.json".into())?;
    ///
    /// assert_eq(
    ///   foo_relative.some_path,
    ///   FileRelativePathBuf::FileRelative("some/file.json".into())
    /// );
    /// ```
    fn make_paths_relative_to_file(
        self,
        path_to_containing_file: impl AsRef<Utf8Path>,
    ) -> Result<Self>
    where
        Self: Sized,
    {
        let dir = get_file_parent_dir(path_to_containing_file.as_ref())?;
        self.make_paths_relative_to_dir(&dir)
    }

    /// Convert all the FileRelativePathBufs in this struct (recursively) into
    /// the FileRelative state, using the path to the directory of the file that
    /// will contain the file-relative paths.
    ///
    /// This is the function that needs to be implemented by structs.
    ///
    /// ```
    /// struct Foo {
    ///   some_path: FileRelativePathBuf,
    /// }
    /// // trait impl omitted for brevity.
    ///
    /// let foo = Foo {
    ///   some_path: FileRelativePathBuf::FileRelative("some/file.json".into())
    /// };
    /// let foo_relative = foo.make_paths_relative_to_file("path/to".into())?;
    ///
    /// assert_eq(
    ///   foo_relative.some_path,
    ///   FileRelativePathBuf::FileRelative("some/file.json".into())
    /// );
    /// ```
    fn make_paths_relative_to_dir(
        self,
        path_to_containing_dir: impl AsRef<Utf8Path>,
    ) -> Result<Self>
    where
        Self: Sized;
}

/// A helper function to get the parent of a Utf8Path, or return an error
/// that the given path doesn't have a parent.
fn get_file_parent_dir(file_path: impl AsRef<Utf8Path>) -> Result<Utf8PathBuf> {
    file_path.as_ref().parent().map(Into::into).ok_or_else(|| {
        anyhow!("The given file path does not have a parent: {}", file_path.as_ref())
    })
}

// Implement SupportsFileRelativePaths for FileRelativePathBuf so there is
// has an implementation that satisfies Option<T: SupportsFileRelativePaths> for
// Option<FileRelativePathBuf>
impl SupportsFileRelativePaths for FileRelativePathBuf {
    fn resolve_paths_from_dir(self, dir_path: impl AsRef<Utf8Path>) -> Result<Self>
    where
        Self: Sized,
    {
        self.resolve_from_dir(dir_path)
    }

    fn make_paths_relative_to_dir(
        self,
        path_to_containing_dir: impl AsRef<Utf8Path>,
    ) -> Result<Self>
    where
        Self: Sized,
    {
        self.make_relative_to_dir(path_to_containing_dir)
    }
}

//
// Serialization / Deserialization implementations for FileRelativePathBuf
//

/// This struct is used by serde to perform the Serialization and Deserialization
/// of the FileRelativePathBuf.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct FileRelativePathBufSerializationHelper(Utf8PathBuf);

impl From<FileRelativePathBufSerializationHelper> for FileRelativePathBuf {
    fn from(value: FileRelativePathBufSerializationHelper) -> Self {
        FileRelativePathBuf::FileRelative(value.0)
    }
}

impl Into<FileRelativePathBufSerializationHelper> for FileRelativePathBuf {
    fn into(self) -> FileRelativePathBufSerializationHelper {
        FileRelativePathBufSerializationHelper(self.into())
    }
}

//
// Conversion implementations for FileRelativePathBuf
//

// The FileRelativePathBuf can be converted into a standard Utf8PathBuf in either
// form.
impl From<FileRelativePathBuf> for Utf8PathBuf {
    fn from(value: FileRelativePathBuf) -> Self {
        value.to_utf8_pathbuf()
    }
}

impl AsRef<Utf8Path> for FileRelativePathBuf {
    fn as_ref(&self) -> &Utf8Path {
        self.as_utf8_pathbuf()
    }
}

impl AsRef<std::path::Path> for FileRelativePathBuf {
    fn as_ref(&self) -> &std::path::Path {
        self.as_utf8_pathbuf().as_std_path()
    }
}

// Creating a FileRelativePathBuf from a Utf8PathBuf or a string of some kind
// will always create it in the resolved state (since this is likely the format
// that those paths will be in).
impl From<Utf8PathBuf> for FileRelativePathBuf {
    fn from(value: Utf8PathBuf) -> Self {
        Self::Resolved(value)
    }
}

impl From<String> for FileRelativePathBuf {
    fn from(string: String) -> FileRelativePathBuf {
        FileRelativePathBuf::Resolved(string.into())
    }
}

impl std::str::FromStr for FileRelativePathBuf {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(FileRelativePathBuf::Resolved(s.into()))
    }
}

impl<T: ?Sized + AsRef<str>> From<&T> for FileRelativePathBuf {
    fn from(s: &T) -> FileRelativePathBuf {
        FileRelativePathBuf::from(s.as_ref().to_owned())
    }
}

impl std::fmt::Display for FileRelativePathBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        AsRef::<Utf8Path>::as_ref(self).fmt(f)
    }
}

//
// Trait implementations for various containers of FileRelativePathBuf
//

impl<T: SupportsFileRelativePaths> SupportsFileRelativePaths for Vec<T> {
    fn resolve_paths_from_dir(self, dir_path: impl AsRef<Utf8Path>) -> Result<Self> {
        self.into_iter().map(|p| p.resolve_paths_from_dir(&dir_path)).collect()
    }

    fn make_paths_relative_to_dir(
        self,
        path_to_containing_dir: impl AsRef<Utf8Path>,
    ) -> Result<Self> {
        self.into_iter().map(|p| p.make_paths_relative_to_dir(&path_to_containing_dir)).collect()
    }
}

impl<T: SupportsFileRelativePaths> SupportsFileRelativePaths for Option<T> {
    fn resolve_paths_from_dir(self, dir_path: impl AsRef<Utf8Path>) -> Result<Self> {
        self.map(|s| s.resolve_paths_from_dir(dir_path)).transpose()
    }

    fn make_paths_relative_to_dir(
        self,
        path_to_containing_dir: impl AsRef<Utf8Path>,
    ) -> Result<Self> {
        self.map(|s| s.make_paths_relative_to_dir(path_to_containing_dir)).transpose()
    }
}

impl<K: std::cmp::Ord, V: SupportsFileRelativePaths> SupportsFileRelativePaths for BTreeMap<K, V> {
    fn resolve_paths_from_dir(self, dir_path: impl AsRef<Utf8Path>) -> Result<Self>
    where
        Self: Sized,
    {
        self.into_iter()
            .map(|(k, v)| v.resolve_paths_from_dir(&dir_path).map(|resolved| (k, resolved)))
            .collect()
    }

    fn make_paths_relative_to_dir(
        self,
        path_to_containing_dir: impl AsRef<Utf8Path>,
    ) -> Result<Self>
    where
        Self: Sized,
    {
        self.into_iter()
            .map(|(k, v)| {
                v.make_paths_relative_to_dir(&path_to_containing_dir).map(|relative| (k, relative))
            })
            .collect()
    }
}

impl<K: std::cmp::Eq + std::hash::Hash, V: SupportsFileRelativePaths> SupportsFileRelativePaths
    for HashMap<K, V>
{
    fn resolve_paths_from_dir(self, dir_path: impl AsRef<Utf8Path>) -> Result<Self>
    where
        Self: Sized,
    {
        self.into_iter()
            .map(|(k, v)| v.resolve_paths_from_dir(&dir_path).map(|resolved| (k, resolved)))
            .collect()
    }

    fn make_paths_relative_to_dir(
        self,
        path_to_containing_dir: impl AsRef<Utf8Path>,
    ) -> Result<Self>
    where
        Self: Sized,
    {
        self.into_iter()
            .map(|(k, v)| {
                v.make_paths_relative_to_dir(&path_to_containing_dir).map(|relative| (k, relative))
            })
            .collect()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::str::FromStr;

    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    struct SimpleStruct {
        flag: bool,
        path: FileRelativePathBuf,
    }

    impl SupportsFileRelativePaths for SimpleStruct {
        fn resolve_paths_from_dir(self, path_to_dir: impl AsRef<Utf8Path>) -> Result<Self> {
            Ok(Self { path: self.path.resolve_from_dir(path_to_dir)?, ..self })
        }

        fn make_paths_relative_to_dir(
            self,
            path_to_containing_dir: impl AsRef<Utf8Path>,
        ) -> Result<Self> {
            Ok(Self { path: self.path.make_relative_to_dir(path_to_containing_dir)?, ..self })
        }
    }

    #[test]
    fn test_standard_usage_simple_struct() {
        let json = serde_json::json!({
          "flag": true,
          "path":  "foo/file_1.json"
        });

        // The parsed file uses "file-relative" paths
        let parsed: SimpleStruct = serde_json::from_value(json).unwrap();
        assert_eq!(
            &parsed,
            &SimpleStruct {
                flag: true,
                path: FileRelativePathBuf::FileRelative("foo/file_1.json".into())
            }
        );

        // After resolving, they will be "resolved" paths
        let resolved = parsed.resolve_paths_from_file("resources/manifest.json").unwrap();
        assert_eq!(
            resolved,
            SimpleStruct {
                flag: true,
                path: FileRelativePathBuf::Resolved("resources/foo/file_1.json".into())
            }
        );

        // When created from paths/strings, they will start as "resolved"
        let created = SimpleStruct { flag: false, path: "resources/foo/file_1.json".into() };
        assert_eq!(
            &created,
            &SimpleStruct {
                flag: false,
                path: FileRelativePathBuf::Resolved("resources/foo/file_1.json".into())
            }
        );

        // And then can be made relative
        let relative = created.make_paths_relative_to_file("resources/manifest.json").unwrap();
        assert_eq!(
            relative,
            SimpleStruct {
                flag: false,
                path: FileRelativePathBuf::FileRelative("foo/file_1.json".into())
            }
        );

        // The re-serialized value should be the same as it originally was.
        let serialized = serde_json::to_value(relative).unwrap();
        assert_eq!(
            serialized,
            serde_json::json!({
                "flag": false,
                "path":  "foo/file_1.json"
            })
        )
    }

    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    struct ParentStruct {
        name: String,
        some_path: FileRelativePathBuf,
        child: SimpleStruct,
    }

    impl SupportsFileRelativePaths for ParentStruct {
        fn resolve_paths_from_dir(self, path_to_dir: impl AsRef<Utf8Path>) -> Result<Self> {
            Ok(Self {
                // Convert the path in this struct
                some_path: self.some_path.resolve_from_dir(&path_to_dir)?,

                // Convert the fields that implement this trait
                child: self.child.resolve_paths_from_dir(&path_to_dir)?,

                // and forward the rest of the fields
                ..self
            })
        }

        fn make_paths_relative_to_dir(self, path_to_dir: impl AsRef<Utf8Path>) -> Result<Self> {
            Ok(Self {
                // Convert the path in this struct
                some_path: self.some_path.make_relative_to_dir(&path_to_dir)?,

                // Convert the fields that implement this trait
                child: self.child.make_paths_relative_to_dir(&path_to_dir)?,

                // and forward the rest of the fields
                ..self
            })
        }
    }

    #[test]
    fn test_standard_usage_nested_struct() {
        let json = serde_json::json!({
          "name": "A name",
          "some_path": "bar/file_2.json",
          "child": {
            "flag": true,
            "path":  "foo/file_1.json"
          }
        });

        // The parsed file uses "file-relative" paths
        let parsed: ParentStruct = serde_json::from_value(json).unwrap();
        assert_eq!(
            &parsed,
            &ParentStruct {
                name: "A name".into(),
                some_path: FileRelativePathBuf::FileRelative("bar/file_2.json".into()),
                child: SimpleStruct {
                    flag: true,
                    path: FileRelativePathBuf::FileRelative("foo/file_1.json".into())
                }
            }
        );

        // And then can be resolved:
        let resolved = parsed.resolve_paths_from_file("resources/manifest.json").unwrap();
        assert_eq!(
            resolved,
            ParentStruct {
                name: "A name".into(),
                some_path: FileRelativePathBuf::Resolved("resources/bar/file_2.json".into()),
                child: SimpleStruct {
                    flag: true,
                    path: FileRelativePathBuf::Resolved("resources/foo/file_1.json".into())
                }
            }
        );

        // Create a new one (say after copying):
        let created = ParentStruct {
            name: "A name".into(),
            some_path: "some/new/location/bar/file_2.json".into(),
            child: SimpleStruct { flag: true, path: "some/new/location/foo/file_1.json".into() },
        };
        assert_eq!(
            created.some_path,
            FileRelativePathBuf::Resolved("some/new/location/bar/file_2.json".into())
        );
        assert_eq!(
            created.child.path,
            FileRelativePathBuf::Resolved("some/new/location/foo/file_1.json".into())
        );

        // And make relative again:
        let relative =
            created.make_paths_relative_to_file("some/new/location/outputs.json").unwrap();
        assert_eq!(
            relative,
            ParentStruct {
                name: "A name".into(),
                some_path: FileRelativePathBuf::FileRelative("bar/file_2.json".into()),
                child: SimpleStruct {
                    flag: true,
                    path: FileRelativePathBuf::FileRelative("foo/file_1.json".into())
                }
            }
        );

        // And then serialize:
        let serialzed = serde_json::to_value(relative).unwrap();
        assert_eq!(
            serialzed,
            serde_json::json!({
                "name": "A name",
                "some_path": "bar/file_2.json",
                "child": {
                    "flag": true,
                    "path":  "foo/file_1.json"
                }
            })
        );
    }

    #[derive(Deserialize)]
    struct StructWithVec {
        paths: Vec<FileRelativePathBuf>,
    }

    impl SupportsFileRelativePaths for StructWithVec {
        fn resolve_paths_from_dir(self, dir_path: impl AsRef<Utf8Path>) -> Result<Self>
        where
            Self: Sized,
        {
            Ok(Self { paths: self.paths.resolve_paths_from_dir(dir_path)? })
        }

        fn make_paths_relative_to_dir(
            self,
            path_to_containing_dir: impl AsRef<Utf8Path>,
        ) -> Result<Self>
        where
            Self: Sized,
        {
            Ok(Self { paths: self.paths.make_paths_relative_to_dir(path_to_containing_dir)? })
        }
    }

    #[test]
    fn test_trait_impl_vec_resolve() {
        let json = serde_json::json!({
          "paths": [
            "file_1",
            "file_2",
            "file_3"
          ]
        });

        let parsed: StructWithVec = serde_json::from_value(json).unwrap();
        assert_eq!(
            &parsed.paths,
            &vec![
                FileRelativePathBuf::FileRelative("file_1".into()),
                FileRelativePathBuf::FileRelative("file_2".into()),
                FileRelativePathBuf::FileRelative("file_3".into()),
            ]
        );

        let resolved = parsed.resolve_paths_from_file("some/manifest").unwrap();
        assert_eq!(
            &resolved.paths,
            &vec![
                FileRelativePathBuf::Resolved("some/file_1".into()),
                FileRelativePathBuf::Resolved("some/file_2".into()),
                FileRelativePathBuf::Resolved("some/file_3".into()),
            ]
        );
    }

    #[test]
    fn test_trait_impl_vec_make_relative() {
        let created = StructWithVec {
            paths: vec!["some/file_1".into(), "some/file_2".into(), "some/file_3".into()],
        };

        let relative = created.make_paths_relative_to_file("some/other_manifest").unwrap();
        assert_eq!(
            &relative.paths,
            &vec![
                FileRelativePathBuf::FileRelative("file_1".into()),
                FileRelativePathBuf::FileRelative("file_2".into()),
                FileRelativePathBuf::FileRelative("file_3".into()),
            ]
        );
    }

    #[test]
    fn test_trait_impl_option_resolve() {
        let option = Some(SimpleStruct {
            flag: true,
            path: FileRelativePathBuf::FileRelative("a/file.json".into()),
        });
        let resolved = option.resolve_paths_from_file("some/manifest.list").unwrap();
        assert_eq!(
            resolved,
            Some(SimpleStruct {
                flag: true,
                path: FileRelativePathBuf::Resolved("some/a/file.json".into())
            })
        );
    }

    #[test]
    fn test_trait_impl_option_make_relative() {
        let option = Some(SimpleStruct {
            flag: true,
            path: FileRelativePathBuf::Resolved("some/a/file.json".into()),
        });
        let resolved = option.make_paths_relative_to_file("some/manifest.list").unwrap();
        assert_eq!(
            resolved,
            Some(SimpleStruct {
                flag: true,
                path: FileRelativePathBuf::FileRelative("a/file.json".into())
            })
        );
    }

    #[test]
    fn test_trait_resolve_usage() {
        let parsed = SimpleStruct {
            flag: true,
            path: FileRelativePathBuf::FileRelative("foo/file_1.json".into()),
        };
        let resolved = parsed.resolve_paths_from_file("resources/manifest.json").unwrap();
        assert_eq!(
            resolved,
            SimpleStruct {
                flag: true,
                path: FileRelativePathBuf::Resolved("resources/foo/file_1.json".into())
            }
        );
    }

    #[test]
    fn test_trait_make_relative_usage() {
        let original = SimpleStruct { flag: true, path: "resources/foo/file_1.json".into() };
        let relative = original.make_paths_relative_to_file("resources/manifest.json").unwrap();
        assert_eq!(
            relative,
            SimpleStruct {
                flag: true,
                path: FileRelativePathBuf::FileRelative("foo/file_1.json".into())
            }
        )
    }

    #[test]
    fn test_field_from_str() {
        // Validate that when creating from a string, it's always a 'resolved' path.
        let created = FileRelativePathBuf::from_str("resources/foo/file_2.json").unwrap();
        assert_eq!(created, FileRelativePathBuf::Resolved("resources/foo/file_2.json".into()));
    }

    #[test]
    fn test_field_from_string() {
        let created = FileRelativePathBuf::from(&String::from("resources/foo/file_2.json"));
        assert_eq!(created, FileRelativePathBuf::Resolved("resources/foo/file_2.json".into()))
    }

    #[test]
    fn test_field_resolve_from_dir() {
        let parsed = FileRelativePathBuf::FileRelative("foo/file_2.json".into());
        let resolved = parsed.resolve_from_dir("resources").unwrap();
        assert_eq!(resolved, FileRelativePathBuf::Resolved("resources/foo/file_2.json".into()));
    }

    #[test]
    fn test_field_make_relative_to_file() {
        let original = FileRelativePathBuf::from("foo/bar/baz.json");
        let relative = original.make_relative_to_file("foo/file.json").unwrap();
        assert_eq!(relative, FileRelativePathBuf::FileRelative("bar/baz.json".into()))
    }

    #[test]
    fn test_field_make_relative_to_dir() {
        let original = FileRelativePathBuf::from("foo/bar/baz.json");
        let relative = original.make_relative_to_dir("foo").unwrap();
        assert_eq!(relative, FileRelativePathBuf::FileRelative("bar/baz.json".into()))
    }

    #[test]
    fn test_display() {
        let original = FileRelativePathBuf::from("foo/bar/baz.json");
        assert_eq!(format!("{}", original), "foo/bar/baz.json");

        let relative = original.make_relative_to_dir("foo").unwrap();
        assert_eq!(format!("{}", relative), "bar/baz.json");
    }
}
