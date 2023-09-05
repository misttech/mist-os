// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ::namespace::{Entry as NamespaceEntry, EntryError, Path as NamespacePath};
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_runner as fcrunner;
use fidl_fuchsia_io as fio;
use thiserror::Error;

/// The namespace of a component instance.
pub struct Namespace {
    pub entries: Vec<NamespaceEntry>,
}

impl Namespace {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn add(&mut self, path: NamespacePath, directory: ClientEnd<fio::DirectoryMarker>) {
        self.entries.push(NamespaceEntry { path, directory });
    }

    /// Adds entries to the namespace, returning an error if any of the paths overlap.
    pub fn merge(&mut self, mut entries: Vec<NamespaceEntry>) -> Result<(), NamespaceError> {
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

    fn is_path_conflict(path_1: &NamespacePath, path_2: &NamespacePath) -> bool {
        path_1.as_str().starts_with(path_2.as_str()) || path_2.as_str().starts_with(path_1.as_str())
    }
}

impl Default for Namespace {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Vec<NamespaceEntry>> for Namespace {
    fn from(entries: Vec<NamespaceEntry>) -> Self {
        Self { entries }
    }
}

impl From<Namespace> for Vec<NamespaceEntry> {
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
