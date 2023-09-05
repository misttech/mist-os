// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! `namespace` defines namespace types and transformations between their common representations.

use fidl::endpoints::ClientEnd;
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_runner as fcrunner;
use fidl_fuchsia_io as fio;
use thiserror::Error;

mod path;
pub use path::{Path, PathError};

/// A container for a single namespace entry, containing a path and a directory handle.
#[derive(Eq, Ord, PartialOrd, PartialEq, Debug)]
pub struct Entry {
    /// Namespace path.
    pub path: Path,

    /// Namespace directory handle.
    pub directory: ClientEnd<fio::DirectoryMarker>,
}

impl From<Entry> for fcrunner::ComponentNamespaceEntry {
    fn from(entry: Entry) -> Self {
        Self {
            path: Some(entry.path.into()),
            directory: Some(entry.directory),
            ..Default::default()
        }
    }
}

impl From<Entry> for fcomponent::NamespaceEntry {
    fn from(entry: Entry) -> Self {
        Self {
            path: Some(entry.path.into()),
            directory: Some(entry.directory),
            ..Default::default()
        }
    }
}

#[cfg(target_os = "fuchsia")]
impl From<Entry> for process_builder::NamespaceEntry {
    fn from(entry: Entry) -> Self {
        Self { path: entry.path.into(), directory: entry.directory }
    }
}

#[derive(Debug, Clone, Error)]
pub enum EntryError {
    #[error("path is not set")]
    MissingPath,

    #[error("directory is not set")]
    MissingDirectory,

    #[error("path is invalid for a namespace entry: `{0}`")]
    InvalidPath(#[from] PathError),
}

impl TryFrom<fcrunner::ComponentNamespaceEntry> for Entry {
    type Error = EntryError;

    fn try_from(entry: fcrunner::ComponentNamespaceEntry) -> Result<Self, Self::Error> {
        Ok(Self {
            path: entry.path.ok_or_else(|| EntryError::MissingPath)?.try_into()?,
            directory: entry.directory.ok_or_else(|| EntryError::MissingDirectory)?,
        })
    }
}

impl TryFrom<fcomponent::NamespaceEntry> for Entry {
    type Error = EntryError;

    fn try_from(entry: fcomponent::NamespaceEntry) -> Result<Self, Self::Error> {
        Ok(Self {
            path: entry.path.ok_or_else(|| EntryError::MissingPath)?.try_into()?,
            directory: entry.directory.ok_or_else(|| EntryError::MissingDirectory)?,
        })
    }
}

#[cfg(target_os = "fuchsia")]
impl TryFrom<process_builder::NamespaceEntry> for Entry {
    type Error = EntryError;

    fn try_from(entry: process_builder::NamespaceEntry) -> Result<Self, Self::Error> {
        Ok(Self { path: entry.path.try_into()?, directory: entry.directory })
    }
}
