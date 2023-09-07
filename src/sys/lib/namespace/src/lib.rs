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
mod tree;
pub use path::{Path, PathError};
pub use tree::Tree;

/// The namespace of a component instance.
///
/// Namespaces may be represented as a collection of directory client endpoints and their
/// corresponding unique paths, so called "flat representation". In this case each path
/// must be a valid [`Path`], and no path can be a parent of another path.
///
/// See https://fuchsia.dev/fuchsia-src/concepts/process/namespaces.
#[derive(Debug)]
pub struct Namespace {
    tree: Tree<ClientEnd<fio::DirectoryMarker>>,
}

#[derive(Error, Debug, Clone)]
pub enum NamespaceError {
    #[error(
        "path `{0}` is the parent or child of another namespace entry. \
        This is not supported."
    )]
    Shadow(Path),

    #[error("duplicate namespace path `{0}`")]
    Duplicate(Path),

    #[error("invalid namespace entry `{0}`")]
    EntryError(#[from] EntryError),
}

impl Namespace {
    pub fn new() -> Self {
        Self { tree: Default::default() }
    }

    pub fn add(
        &mut self,
        path: &Path,
        directory: ClientEnd<fio::DirectoryMarker>,
    ) -> Result<(), NamespaceError> {
        self.tree.add(path, directory)?;
        Ok(())
    }

    pub fn remove(&mut self, path: &Path) -> Option<ClientEnd<fio::DirectoryMarker>> {
        self.tree.remove(path)
    }

    pub fn flatten(self) -> Vec<Entry> {
        self.tree.flatten().into_iter().map(|(path, directory)| Entry { path, directory }).collect()
    }

    /// Get a copy of the paths in the namespace.
    pub fn paths(&self) -> Vec<Path> {
        self.tree.map_ref(|_| ()).clone().flatten().into_iter().map(|(path, ())| path).collect()
    }
}

impl Default for Namespace {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Tree<ClientEnd<fio::DirectoryMarker>>> for Namespace {
    fn from(tree: Tree<ClientEnd<fio::DirectoryMarker>>) -> Self {
        Self { tree }
    }
}

#[cfg(target_os = "fuchsia")]
impl Clone for Namespace {
    fn clone(&self) -> Self {
        use fidl::AsHandleRef;
        use fuchsia_zircon as zx;

        // TODO(fxbug.dev/132998): The unsafe block can go away if Rust FIDL bindings exposed the
        // feature of calling FIDL methods (e.g. Clone) on a borrowed client endpoint.
        let tree = self.tree.map_ref(|dir| {
            let raw_handle = dir.channel().as_handle_ref().raw_handle();
            // SAFETY: the channel is forgotten at the end of scope so it is not double closed.
            unsafe {
                let borrowed: zx::Channel = zx::Handle::from_raw(raw_handle).into();
                let borrowed = fio::DirectorySynchronousProxy::new(borrowed);
                let (client_end, server_end) = fidl::endpoints::create_endpoints();
                let _ = borrowed.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server_end);
                std::mem::forget(borrowed.into_channel());
                client_end.into_channel().into()
            }
        });
        Self { tree }
    }
}

impl TryFrom<Vec<Entry>> for Namespace {
    type Error = NamespaceError;

    fn try_from(value: Vec<Entry>) -> Result<Self, Self::Error> {
        let mut ns = Namespace::new();
        for entry in value {
            ns.add(&entry.path, entry.directory)?;
        }
        Ok(ns)
    }
}

impl From<Namespace> for Vec<Entry> {
    fn from(namespace: Namespace) -> Self {
        namespace.flatten()
    }
}

impl From<Namespace> for Vec<fcrunner::ComponentNamespaceEntry> {
    fn from(namespace: Namespace) -> Self {
        namespace.flatten().into_iter().map(Into::into).collect()
    }
}

#[cfg(target_os = "fuchsia")]
impl From<Namespace> for Vec<process_builder::NamespaceEntry> {
    fn from(namespace: Namespace) -> Self {
        namespace.flatten().into_iter().map(Into::into).collect()
    }
}

impl TryFrom<Vec<fcrunner::ComponentNamespaceEntry>> for Namespace {
    type Error = NamespaceError;

    fn try_from(entries: Vec<fcrunner::ComponentNamespaceEntry>) -> Result<Self, Self::Error> {
        let entries: Vec<Entry> =
            entries.into_iter().map(TryInto::try_into).collect::<Result<Vec<_>, EntryError>>()?;
        entries.try_into()
    }
}

impl TryFrom<Vec<fcomponent::NamespaceEntry>> for Namespace {
    type Error = NamespaceError;

    fn try_from(entries: Vec<fcomponent::NamespaceEntry>) -> Result<Self, Self::Error> {
        let entries: Vec<Entry> =
            entries.into_iter().map(TryInto::try_into).collect::<Result<Vec<_>, EntryError>>()?;
        entries.try_into()
    }
}

#[cfg(target_os = "fuchsia")]
impl TryFrom<Vec<process_builder::NamespaceEntry>> for Namespace {
    type Error = NamespaceError;

    fn try_from(entries: Vec<process_builder::NamespaceEntry>) -> Result<Self, Self::Error> {
        let entries: Vec<Entry> =
            entries.into_iter().map(TryInto::try_into).collect::<Result<Vec<_>, EntryError>>()?;
        entries.try_into()
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::ns_path;
    use assert_matches::assert_matches;
    use fuchsia_async as fasync;
    use fuchsia_zircon as zx;
    use zx::{AsHandleRef, Peered};

    #[test]
    fn test_try_from_namespace() {
        {
            let (client_end_1, _) = fidl::endpoints::create_endpoints();
            let (client_end_2, _) = fidl::endpoints::create_endpoints();
            let entries = vec![
                Entry { path: ns_path("/foo"), directory: client_end_1 },
                Entry { path: ns_path("/foo/bar"), directory: client_end_2 },
            ];
            assert_matches!(
                Namespace::try_from(entries),
                Err(NamespaceError::Shadow(path)) if path.as_str() == "/foo/bar"
            );
        }
        {
            let (client_end_1, _) = fidl::endpoints::create_endpoints();
            let (client_end_2, _) = fidl::endpoints::create_endpoints();
            let entries = vec![
                Entry { path: ns_path("/foo"), directory: client_end_1 },
                Entry { path: ns_path("/foo"), directory: client_end_2 },
            ];
            assert_matches!(
                Namespace::try_from(entries),
                Err(NamespaceError::Duplicate(path)) if path.as_str() == "/foo"
            );
        }
    }

    #[cfg(target_os = "fuchsia")]
    #[fasync::run_singlethreaded(test)]
    async fn test_clone() {
        use vfs::{
            directory::entry::DirectoryEntry, execution_scope::ExecutionScope, file::vmo::read_only,
        };

        // Set up a directory server.
        let dir = vfs::pseudo_directory! {
            "foo" => vfs::pseudo_directory! {
                "bar" => read_only(b"Fuchsia"),
            },
        };
        let (client_end, server_end) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
        let scope = ExecutionScope::new();
        dir.open(
            scope,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot(),
            fidl::endpoints::ServerEnd::new(server_end.into()),
        );

        // Make a namespace pointing to that server.
        let mut namespace = Namespace::new();
        namespace.add(&ns_path("/data"), client_end).unwrap();

        // Both this namespace and the clone should point to the same server.
        let namespace_clone = namespace.clone();

        let mut entries = namespace.flatten();
        let mut entries_clone = namespace_clone.flatten();

        async fn verify(entry: Entry) {
            assert_eq!(entry.path.as_str(), "/data");
            let dir = entry.directory.into_proxy().unwrap();
            let file =
                fuchsia_fs::directory::open_file(&dir, "foo/bar", fio::OpenFlags::RIGHT_READABLE)
                    .await
                    .unwrap();
            let content = fuchsia_fs::file::read(&file).await.unwrap();
            assert_eq!(content, b"Fuchsia");
        }

        verify(entries.remove(0)).await;
        verify(entries_clone.remove(0)).await;
    }

    #[test]
    fn test_flatten() {
        let mut namespace = Namespace::new();
        let (client_end, server_end) = fidl::endpoints::create_endpoints();
        namespace.add(&ns_path("/svc"), client_end).unwrap();
        let mut entries = namespace.flatten();
        assert_eq!(entries[0].path.as_str(), "/svc");
        entries
            .remove(0)
            .directory
            .into_channel()
            .signal_peer(zx::Signals::empty(), zx::Signals::USER_0)
            .unwrap();
        server_end.wait_handle(zx::Signals::USER_0, zx::Time::INFINITE).unwrap();
    }

    #[test]
    fn test_remove() {
        let mut namespace = Namespace::new();
        let (client_end, server_end) = fidl::endpoints::create_endpoints();
        namespace.add(&ns_path("/svc"), client_end).unwrap();
        let client_end = namespace.remove(&ns_path("/svc")).unwrap();
        let entries = namespace.flatten();
        assert!(entries.is_empty());
        client_end.into_channel().signal_peer(zx::Signals::empty(), zx::Signals::USER_0).unwrap();
        server_end.wait_handle(zx::Signals::USER_0, zx::Time::INFINITE).unwrap();
    }
}
