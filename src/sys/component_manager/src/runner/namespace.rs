// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    fidl::endpoints::ClientEnd, fidl_fuchsia_component as fcomponent,
    fidl_fuchsia_component_runner as fcrunner, fidl_fuchsia_io as fio, thiserror::Error,
};

/// The namespace of a component instance.
pub struct Namespace {
    pub entries: Vec<Entry>,
}

impl Namespace {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn add(&mut self, path: String, directory: ClientEnd<fio::DirectoryMarker>) {
        self.entries.push(Entry { path, directory });
    }

    /// Adds entries to the namespace, returning an error if any of the paths overlap.
    pub fn merge(&mut self, mut entries: Vec<Entry>) -> Result<(), NamespaceError> {
        for existing_entry in &self.entries {
            if entries
                .iter()
                .any(|new_entry| Namespace::is_path_conflict(&existing_entry.path, &new_entry.path))
            {
                return Err(NamespaceError::PathConflict);
            }
        }
        self.entries.append(&mut entries);
        Ok(())
    }

    fn is_path_conflict(path_1: &String, path_2: &String) -> bool {
        path_1.starts_with(path_2) || path_2.starts_with(path_1)
    }
}

impl Default for Namespace {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Vec<Entry>> for Namespace {
    fn from(entries: Vec<Entry>) -> Self {
        Self { entries }
    }
}

impl From<Namespace> for Vec<Entry> {
    fn from(namespace: Namespace) -> Self {
        namespace.entries
    }
}

impl From<Namespace> for Vec<fcrunner::ComponentNamespaceEntry> {
    fn from(namespace: Namespace) -> Self {
        namespace.entries.into_iter().map(Into::into).collect()
    }
}

#[derive(Debug, Clone, Error)]
pub enum NamespaceError {
    #[error("invalid entry")]
    EntryError(#[source] EntryError),

    #[error("path conflicts with existing path")]
    PathConflict,
}

impl TryFrom<Vec<fcrunner::ComponentNamespaceEntry>> for Namespace {
    type Error = NamespaceError;

    fn try_from(entries: Vec<fcrunner::ComponentNamespaceEntry>) -> Result<Self, Self::Error> {
        let entries = entries
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, EntryError>>()
            .map_err(NamespaceError::EntryError)?;
        Ok(Self { entries })
    }
}

impl TryFrom<Vec<fcomponent::NamespaceEntry>> for Namespace {
    type Error = NamespaceError;

    fn try_from(entries: Vec<fcomponent::NamespaceEntry>) -> Result<Self, Self::Error> {
        let entries = entries
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, EntryError>>()
            .map_err(NamespaceError::EntryError)?;
        Ok(Self { entries })
    }
}

/// A component namespace entry.
pub struct Entry {
    pub path: String,
    pub directory: ClientEnd<fio::DirectoryMarker>,
}

#[derive(Debug, Clone, Error)]
pub enum EntryError {
    #[error("path is not set")]
    MissingPath,

    #[error("directory is not set")]
    MissingDirectory,
}

impl From<Entry> for fcrunner::ComponentNamespaceEntry {
    fn from(entry: Entry) -> Self {
        Self { path: Some(entry.path), directory: Some(entry.directory), ..Default::default() }
    }
}

impl TryFrom<fcrunner::ComponentNamespaceEntry> for Entry {
    type Error = EntryError;

    fn try_from(entry: fcrunner::ComponentNamespaceEntry) -> Result<Self, Self::Error> {
        Ok(Self {
            path: entry.path.ok_or_else(|| EntryError::MissingPath)?,
            directory: entry.directory.ok_or_else(|| EntryError::MissingDirectory)?,
        })
    }
}

impl TryFrom<fcomponent::NamespaceEntry> for Entry {
    type Error = EntryError;

    fn try_from(entry: fcomponent::NamespaceEntry) -> Result<Self, Self::Error> {
        Ok(Self {
            path: entry.path.ok_or_else(|| EntryError::MissingPath)?,
            directory: entry.directory.ok_or_else(|| EntryError::MissingDirectory)?,
        })
    }
}
