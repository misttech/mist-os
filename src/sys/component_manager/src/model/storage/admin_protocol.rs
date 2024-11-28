// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The storage admin protocol is a FIDL protocol that is hosted by the framework for clients to
//! perform privileged operations on isolated storage. Clients can perform tasks such as opening a
//! component's storage or outright deleting it.
//!
//! This API allows clients to perform a limited set of mutable operations on storage, without
//! direct access to the backing directory, with the goal of making it easier for clients to work
//! with isolated storage without needing to understand component_manager's storage layout.

use crate::model::component::{ComponentInstance, WeakComponentInstance};
use crate::model::routing::Route;
use crate::model::storage::{self, BackingDirectoryInfo};
use ::routing::capability_source::{ComponentCapability, ComponentSource};
use ::routing::RouteSource;
use anyhow::{format_err, Context, Error};
use cm_rust::{StorageDecl, UseDecl};
use component_id_index::InstanceId;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io::{self as fio, DirectoryProxy, DirentType};
use fuchsia_fs::directory as ffs_dir;
use futures::stream::{FuturesUnordered, StreamExt};
use futures::{Future, TryFutureExt, TryStreamExt};
use moniker::Moniker;
use routing::capability_source::CapabilitySource;
use routing::component_instance::ComponentInstanceInterface;
use routing::RouteRequest;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, error, warn};
use {fidl_fuchsia_component as fcomponent, fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync};

#[derive(Debug, PartialEq)]
enum DirType {
    Component,
    Children,
    ComponentStorage,
    Unknown,
}

#[derive(Debug)]
enum StorageError {
    NoStorageFound,
    Operation(DeletionError),
}

#[derive(Debug)]
enum DeletionError {
    DirectoryRead(ffs_dir::EnumerateError),
    ContentError(Vec<DeletionErrorCause>),
}

impl From<StorageError> for fsys::DeletionError {
    fn from(from: StorageError) -> fsys::DeletionError {
        match from {
            StorageError::NoStorageFound => fsys::DeletionError::NoneAvailable,
            StorageError::Operation(DeletionError::DirectoryRead(
                ffs_dir::EnumerateError::Fidl(_, fidl::Error::ClientChannelClosed { .. }),
            )) => fsys::DeletionError::Connection,
            StorageError::Operation(DeletionError::DirectoryRead(_)) => {
                fsys::DeletionError::Protocol
            }
            StorageError::Operation(DeletionError::ContentError(errors)) => match errors.get(0) {
                None => fsys::DeletionError::Protocol,
                Some(DeletionErrorCause::Directory(dir_read_err)) => match dir_read_err {
                    ffs_dir::EnumerateError::Fidl(_, fidl::Error::ClientChannelClosed { .. }) => {
                        fsys::DeletionError::Connection
                    }
                    _ => fsys::DeletionError::Protocol,
                },
                Some(DeletionErrorCause::File { _error_code }) => fsys::DeletionError::Protocol,
                Some(DeletionErrorCause::FileRequest(fidl::Error::ClientChannelClosed {
                    ..
                })) => fsys::DeletionError::Connection,
                Some(DeletionErrorCause::FileRequest(_)) => fsys::DeletionError::Connection,
            },
        }
    }
}

#[derive(Debug)]
enum DeletionErrorCause {
    /// There was an error removing a directory.
    Directory(ffs_dir::EnumerateError),
    /// The IPC to the I/O server succeeded, but the file operation
    /// returned an error.
    File { _error_code: i32 },
    /// The IPC to the I/O server failed.
    FileRequest(fidl::Error),
}

#[derive(Debug)]
/// Error values returned by StorageAdminProtocolProvider::get_storage_status
enum StorageStatusError {
    /// We encountered an RPC error asking for filesystem info
    QueryError,
    /// We asked the Directory provider for information, but they returned
    /// none, likely the Directory provider is improperly implemented.
    NoFilesystemInfo,
    /// We got information from the Directory provider, but it seems invalid.
    InconsistentInformation,
}

impl From<StorageStatusError> for fsys::StatusError {
    fn from(from: StorageStatusError) -> fsys::StatusError {
        match from {
            StorageStatusError::InconsistentInformation => fsys::StatusError::ResponseInvalid,
            StorageStatusError::NoFilesystemInfo => fsys::StatusError::StatusUnknown,
            StorageStatusError::QueryError => fsys::StatusError::Provider,
        }
    }
}

pub struct StorageAdmin;

impl StorageAdmin {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }

    /// Serves the `fuchsia.sys2/StorageAdmin` protocol over the provided
    /// channel based on the information provided by the other arguments.
    ///
    /// # Arguments
    /// * `storage_decl`: The manifest declaration where the storage
    ///   capability was defined.
    /// * `component`: Reference to the component which defined the storage
    ///   capability.
    /// * `server_end`: Channel to server the protocol over.
    pub async fn serve(
        self: Arc<Self>,
        storage_decl: StorageDecl,
        component: WeakComponentInstance,
        mut stream: fsys::StorageAdminRequestStream,
    ) -> Result<(), Error> {
        let storage_source = RouteSource {
            source: CapabilitySource::Component(ComponentSource {
                capability: ComponentCapability::Storage(storage_decl.clone()),
                moniker: component.moniker.clone(),
            }),
            relative_path: Default::default(),
        };
        let backing_dir_source_info =
            storage::route_backing_directory(&component.upgrade()?, storage_source.source)
                .await
                .context("could not serve storage protocol, routing backing directory failed")?;

        let component = component.upgrade().map_err(|e| {
            format_err!(
                "unable to serve storage admin protocol, model reference is no longer valid: {:?}",
                e,
            )
        })?;

        while let Some(request) = stream.try_next().await? {
            match request {
                #[cfg(any(
                    fuchsia_api_level_less_than = "25",
                    fuchsia_api_level_at_least = "PLATFORM"
                ))]
                fsys::StorageAdminRequest::OpenComponentStorage {
                    relative_moniker,
                    flags,
                    mode,
                    object,
                    control_handle: _,
                } => {
                    let moniker = Moniker::try_from(relative_moniker.as_str())?;
                    let absolute_moniker = component.moniker().concat(&moniker);
                    let instance_id =
                        component.component_id_index().id_for_moniker(&absolute_moniker).cloned();

                    let dir_proxy = storage::open_isolated_storage(
                        &backing_dir_source_info,
                        moniker,
                        instance_id.as_ref(),
                    )
                    .await?;
                    dir_proxy.open(flags, mode, ".", object)?;
                }
                #[cfg(fuchsia_api_level_at_least = "25")]
                fsys::StorageAdminRequest::OpenStorage { relative_moniker, object, responder } => {
                    let fut = async {
                        let moniker = Moniker::parse_str(&relative_moniker)
                            .map_err(|_| fcomponent::Error::InvalidArguments)?;
                        let absolute_moniker = component.moniker().concat(&moniker);
                        let instance_id = component
                            .component_id_index()
                            .id_for_moniker(&absolute_moniker)
                            .cloned();
                        let directory = storage::open_isolated_storage(
                            &backing_dir_source_info,
                            moniker,
                            instance_id.as_ref(),
                        )
                        .await
                        .map_err(|_| fcomponent::Error::Internal)?;
                        directory
                            .clone2(object.into_channel().into())
                            .map_err(|_| fcomponent::Error::Internal)?;
                        Ok(())
                    };
                    responder.send(fut.await)?;
                }
                fsys::StorageAdminRequest::ListStorageInRealm {
                    relative_moniker,
                    iterator,
                    responder,
                } => {
                    let fut = async {
                        let relative_moniker = Moniker::parse_str(&relative_moniker)
                            .map_err(|_| fcomponent::Error::InvalidArguments)?;
                        let root_component = component
                            .find_and_maybe_resolve(&relative_moniker)
                            .await
                            .map_err(|_| fcomponent::Error::InstanceNotFound)?;
                        Ok(root_component)
                    };
                    match fut.await {
                        Ok(root_component) => {
                            fasync::Task::spawn(
                                Self::serve_storage_iterator(
                                    root_component,
                                    iterator,
                                    backing_dir_source_info.clone(),
                                )
                                .unwrap_or_else(|error| {
                                    warn!(?error, "Error serving storage iterator")
                                }),
                            )
                            .detach();
                            responder.send(Ok(()))?;
                        }
                        Err(e) => {
                            responder.send(Err(e))?;
                        }
                    }
                }
                fsys::StorageAdminRequest::OpenComponentStorageById { id, object, responder } => {
                    let instance_id_index = component.component_id_index();
                    let Ok(instance_id) = id.parse::<InstanceId>() else {
                        responder.send(Err(fcomponent::Error::InvalidArguments))?;
                        continue;
                    };
                    if !instance_id_index.contains_id(&instance_id) {
                        responder.send(Err(fcomponent::Error::ResourceNotFound))?;
                        continue;
                    }
                    match storage::open_isolated_storage_by_id(
                        &backing_dir_source_info,
                        &instance_id,
                    )
                    .await
                    {
                        Ok(dir) => responder.send(
                            dir.clone2(object.into_channel().into())
                                .map_err(|_| fcomponent::Error::Internal),
                        )?,
                        Err(_) => responder.send(Err(fcomponent::Error::Internal))?,
                    }
                }
                fsys::StorageAdminRequest::DeleteComponentStorage {
                    relative_moniker: moniker_str,
                    responder,
                } => {
                    let parsed_moniker = Moniker::try_from(moniker_str.as_str());
                    let response = match parsed_moniker {
                        Err(error) => {
                            warn!(
                                ?error,
                                "couldn't parse string as moniker for storage admin protocol"
                            );
                            Err(fcomponent::Error::InvalidArguments)
                        }
                        Ok(moniker) => {
                            let absolute_moniker = component.moniker().concat(&moniker);
                            let instance_id = component
                                .component_id_index()
                                .id_for_moniker(&absolute_moniker)
                                .cloned();
                            let res = storage::delete_isolated_storage(
                                backing_dir_source_info.clone(),
                                moniker,
                                instance_id.as_ref(),
                            )
                            .await;
                            match res {
                                Err(e) => {
                                    warn!(
                                        "couldn't delete storage for storage admin protocol: {:?}",
                                        e
                                    );
                                    Err(fcomponent::Error::Internal)
                                }
                                Ok(()) => Ok(()),
                            }
                        }
                    };
                    responder.send(response)?
                }
                fsys::StorageAdminRequest::GetStatus { responder } => {
                    if let Ok(storage_root) =
                        storage::open_storage_root(&backing_dir_source_info).await
                    {
                        responder.send_no_shutdown_on_err(
                            match Self::get_storage_status(&storage_root).await {
                                Ok(ref status) => Ok(status),
                                Err(e) => Err(e.into()),
                            },
                        )?;
                    } else {
                        responder.send_no_shutdown_on_err(Err(fsys::StatusError::Provider))?;
                    }
                }
                fsys::StorageAdminRequest::DeleteAllStorageContents { responder } => {
                    // TODO(handle error properly)
                    if let Ok(storage_root) =
                        storage::open_storage_root(&backing_dir_source_info).await
                    {
                        match Self::delete_all_storage(&storage_root, Self::delete_dir_contents)
                            .await
                        {
                            Ok(_) => responder.send(Ok(()))?,
                            Err(e) => {
                                warn!("errors encountered deleting storage: {:?}", e);
                                responder.send_no_shutdown_on_err(Result::Err(e.into()))?;
                            }
                        }
                    } else {
                        // This might not be _entirely_ accurate, but in this error case we weren't
                        // able to talk to the directory, so that is, in a sense, lack of
                        // connection.
                        responder.send_no_shutdown_on_err(Result::Err(
                            fsys::DeletionError::Connection,
                        ))?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn get_storage_status(
        root_storage: &DirectoryProxy,
    ) -> Result<fsys::StorageStatus, StorageStatusError> {
        let filesystem_info = match root_storage.query_filesystem().await {
            Ok((_, Some(fs_info))) => Ok(fs_info),
            Ok((_, None)) => Err(StorageStatusError::NoFilesystemInfo),
            Err(_) => Err(StorageStatusError::QueryError),
        }?;

        // The number of bytes which may be allocated plus the number of bytes which have been
        // allocated. |total_bytes| is the amount of data (not counting metadata like inode storage)
        // that minfs has currently allocated from the volume manager, while used_bytes is the amount
        // of those actually used for current storage.
        let total_bytes = filesystem_info.free_shared_pool_bytes + filesystem_info.total_bytes;
        if total_bytes == 0 {
            return Err(StorageStatusError::InconsistentInformation);
        }
        if total_bytes < filesystem_info.used_bytes {
            return Err(StorageStatusError::InconsistentInformation);
        }

        Ok(fsys::StorageStatus {
            total_size: Some(total_bytes),
            used_size: Some(filesystem_info.used_bytes),
            ..Default::default()
        })
    }

    /// Deletes the contents of all the subdirectories of |root_storage| which
    /// look like component storage directories. Returns an error if finds no
    /// storage directories underneath |root_storage|.
    async fn delete_all_storage<'a, F, DelFn>(
        root_storage: &'a DirectoryProxy,
        mut del_fn: DelFn,
    ) -> Result<(), StorageError>
    where
        F: Future<Output = Result<(), DeletionError>> + Send + 'static,
        DelFn: FnMut(DirectoryProxy) -> F + Send + 'static + Copy,
    {
        // List the directory, finding all contents
        let mut content_tree = ffs_dir::readdir_recursive_filtered(
            root_storage,
            None,
            |directory: &ffs_dir::DirEntry, _contents: Option<&Vec<ffs_dir::DirEntry>>| {
                directory.kind == DirentType::Directory
            },
            |directory: &ffs_dir::DirEntry| match Self::is_storage_dir(PathBuf::from(
                directory.name.clone(),
            )) {
                (DirType::ComponentStorage, ..) | (DirType::Unknown, ..) => false,
                (DirType::Component, ..) | (DirType::Children, ..) => true,
            },
        );

        let deletions = FuturesUnordered::new();
        while let Some(Ok(entry)) = content_tree.next().await {
            if entry.kind != ffs_dir::DirentKind::Directory {
                continue;
            }

            let path = PathBuf::from(entry.name.clone());

            // For any contents which are directories, see if it is a storage directory
            match Self::is_storage_dir(path) {
                // Open the storage directory and then create a task to delete
                (DirType::ComponentStorage, ..) => {
                    match ffs_dir::open_directory(
                        root_storage,
                        entry.name.as_str(),
                        fio::PERM_READABLE | fio::PERM_WRITABLE,
                    )
                    .await
                    {
                        // Create a task to remove all the directory's contents
                        Ok(storage_dir) => {
                            deletions.push(del_fn(storage_dir));
                        }
                        Err(e) => {
                            warn!("problem opening storage directory: {:?}", e);
                            continue;
                        }
                    }
                }
                (DirType::Component, ..) | (DirType::Children, ..) | (DirType::Unknown, ..) => {
                    // nothing to do for these types
                }
            }
        }

        // wait for any in-progress deletions to complete
        let results = deletions.collect::<Vec<Result<(), DeletionError>>>().await;

        // Seems like we didn't find any storage to clear, which is unexpected
        if results.len() == 0 {
            return Err(StorageError::NoStorageFound);
        }

        for result in results {
            if let Err(e) = result {
                return Err(StorageError::Operation(e));
            }
        }

        Ok(())
    }

    /// Deletes the contents of the directory and recursively deletes any
    /// sub-directories and their contents. Directory contents which are not
    /// a Directory or File are ignored.
    ///
    /// A Result::Err does not necessarily mean nothing was deleted, only that
    /// some errors were encountered, but other deletions may have succeeded.
    ///
    /// Returns DeletionError::DirectoryRead if the top-level directory can not
    /// be read. Returns DeletionError::ContentError if there is a problem
    /// deleting any of the directory files or directories. The ContentError
    /// contains error information for each directory entry for which there was
    /// a problem.
    async fn delete_dir_contents(dir: DirectoryProxy) -> Result<(), DeletionError> {
        let dir_contents = match ffs_dir::readdir(&dir).await {
            Ok(c) => c,
            Err(e) => {
                warn!("Directory failed to list, contents not deleted");
                return Err(DeletionError::DirectoryRead(e));
            }
        };

        let mut errors = vec![];

        for entry in dir_contents {
            match entry.kind {
                ffs_dir::DirentKind::Directory => {
                    match ffs_dir::remove_dir_recursive(&dir, &entry.name).await {
                        Err(e) => errors.push(DeletionErrorCause::Directory(e)),
                        _ => {}
                    }
                }
                ffs_dir::DirentKind::Symlink | ffs_dir::DirentKind::File => {
                    match dir.unlink(&entry.name, &fio::UnlinkOptions::default()).await {
                        Err(e) => errors.push(DeletionErrorCause::FileRequest(e)),
                        Ok(Err(e)) => errors.push(DeletionErrorCause::File { _error_code: e }),
                        _ => {}
                    }
                }
                ffs_dir::DirentKind::BlockDevice
                | ffs_dir::DirentKind::Service
                | ffs_dir::DirentKind::Unknown
                | ffs_dir::DirentKind::__SourceBreaking { .. } => {}
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(DeletionError::ContentError(errors))
        }
    }

    /// For the given PathBuf determines the shortest sub-path that represents
    /// a component storage directory, if any.
    ///
    /// If a storage directory is found the function returns
    /// DirType::ComponentStorage along with the subpath of the storage
    /// directory, and any remaining path.
    ///
    /// If the path represents a path that appears to be part of a moniker-
    /// based storage path, but contains no storage path, the path may be
    /// either a "component" path or a "children" path. A "component" path
    /// might contain a component storage subdirectory. A "children" path
    /// might contain subdirectories for children of a given component where
    /// the children may use storage. In the "component" path and "children"
    /// path case this function returns DirType::Component or
    /// DirType::Children, respectively, a PathBuf that equals the input
    /// PathBuf, and an empty remaining path.
    ///
    /// For a description of how storage directories are structured, see
    /// model::storage::generate_moniker_based_storage_path.
    ///
    /// The function returns DirType::Unknown if:
    /// * The first segment cannot be intrepretted as UTF, since we require this
    ///   to determine if it is a storage ID-type storage directory
    /// * It is child directory of a "component" directory in a moniker-based
    ///   storage path *and* the child directory is not called "data" or
    ///   "children".
    /// * The implementation has a logical error when it finds a
    ///   DirType::ComponentStoragePath, but continues to analyze subpaths.
    fn is_storage_dir(path: PathBuf) -> (DirType, PathBuf, PathBuf) {
        let child_name = "children";
        let data_name = "data";

        // Set the initial state to "unknown", which is sort of true
        let mut prev_segment = DirType::Unknown;
        let mut path_iter = path.iter();
        let mut processed_path = PathBuf::new();

        while let Some(segment) = path_iter.next() {
            processed_path.push(segment);

            match prev_segment {
                // Only for top-level directories do we consider this might be
                // a hex-named, storage ID-based directory.
                DirType::Unknown => {
                    let segment = {
                        if let Some(segment) = segment.to_str() {
                            segment
                        } else {
                            // the conversion failed
                            prev_segment = DirType::Unknown;
                            break;
                        }
                    };

                    // check the string length is 64 and the characters are hex
                    if segment.len() == 64 && segment.chars().all(|c| c.is_ascii_hexdigit()) {
                        prev_segment = DirType::ComponentStorage;
                        break;
                    }

                    // Assume this is a moniker-based path, in which case this
                    // is a "component" directory
                    prev_segment = DirType::Component;
                }
                // Expect that this path segment should match the name for
                // children or data directories
                DirType::Component => {
                    if segment == child_name {
                        prev_segment = DirType::Children;
                    } else if segment == data_name {
                        prev_segment = DirType::ComponentStorage;
                        break;
                    } else {
                        prev_segment = DirType::Unknown;
                        break;
                    }
                }
                // After a child segment, the next directory must be a parent
                // directory of a component. We have no heurstic to know how
                // such a directory might be name, so just assume and hope.
                DirType::Children => prev_segment = DirType::Component,
                // This case represents a logical error, we should always
                // return when we find a ComponentStorage directory, so why are
                // we here?
                DirType::ComponentStorage => {
                    error!(
                        "Function logic error: unexpected value \"ComponentStorage\" for DirType"
                    );
                    return (DirType::Unknown, PathBuf::new(), PathBuf::new());
                }
            }
        }

        // If we arrive here we either
        // * processed the whole apth
        // * found a component storage directory
        // * encountered an unexpected structure
        // * weren't able to convert a path segment to unicode text
        // Collect any remaining path segments and return them along with the variant of the last
        // processed path segment
        let unprocessed_path = {
            let mut remaining = PathBuf::new();
            path_iter.for_each(|path_part| remaining.push(path_part));
            remaining
        };
        (prev_segment, processed_path, unprocessed_path)
    }

    async fn serve_storage_iterator(
        root_component: Arc<ComponentInstance>,
        iterator: ServerEnd<fsys::StorageIteratorMarker>,
        storage_capability_source_info: BackingDirectoryInfo,
    ) -> Result<(), Error> {
        let mut components_to_visit = vec![root_component];
        let mut storage_users = vec![];

        // This is kind of inefficient, it should be possible to follow offers to child once a
        // subtree that has access to the storage is found, rather than checking every single
        // instance's storage uses as done here.
        while let Some(component) = components_to_visit.pop() {
            let component_state = match component.lock_resolved_state().await {
                Ok(state) => state,
                // A component will not have resolved state if it has already been destroyed. In
                // this case, its storage has also been removed, so we should skip it.
                Err(e) => {
                    debug!(
                        "Failed to lock component resolved state, it may already be destroyed: {:?}",
                        e
                    );
                    continue;
                }
            };
            let storage_uses =
                component_state.decl().uses.iter().filter_map(|use_decl| match use_decl {
                    UseDecl::Storage(use_storage) => Some(use_storage),
                    _ => None,
                });
            for use_storage in storage_uses {
                let storage_source =
                    match RouteRequest::UseStorage(use_storage.clone()).route(&component).await {
                        Ok(s) => s,
                        Err(_) => continue,
                    };

                let backing_dir_info =
                    match storage::route_backing_directory(&component, storage_source.source).await
                    {
                        Ok(s) => s,
                        Err(_) => continue,
                    };

                if backing_dir_info == storage_capability_source_info {
                    let moniker = component
                        .moniker()
                        .strip_prefix(&backing_dir_info.storage_source_moniker)
                        .unwrap();
                    storage_users.push(moniker);
                    break;
                }
            }
            for component in component_state.children().map(|(_, v)| v) {
                components_to_visit.push(component.clone())
            }
        }

        const MAX_MONIKERS_RETURNED: usize = 10;
        let mut iterator_stream = iterator.into_stream();
        // TODO(https://fxbug.dev/42157052): This currently returns monikers with instance ids, even though
        // the ListStorageUsers method takes monikers without instance id as arguments. This is done
        // as the Open and Delete methods take monikers with instance id. Once these are updated,
        // ListStorageUsers should also return monikers without instance id.
        let mut storage_users = storage_users.into_iter().map(|moniker| format!("{}", moniker));
        while let Some(request) = iterator_stream.try_next().await? {
            let fsys::StorageIteratorRequest::Next { responder } = request;
            let monikers: Vec<_> = storage_users.by_ref().take(MAX_MONIKERS_RETURNED).collect();
            responder.send(&monikers)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DirType, StorageAdmin, StorageError};
    use fidl::endpoints::ServerEnd;
    use fidl_fuchsia_io as fio;
    use std::fmt::Formatter;
    use std::path::PathBuf;
    use std::sync::Arc;
    use test_case::test_case;
    use vfs::directory::dirents_sink;
    use vfs::directory::entry_container::{Directory, DirectoryWatcher};
    use vfs::directory::immutable::connection::ImmutableConnection;
    use vfs::directory::traversal_position::TraversalPosition;
    use vfs::execution_scope::ExecutionScope;
    use vfs::path::Path;
    use vfs::{ObjectRequestRef, ToObjectRequest};

    #[test_case(
        "aabbccddeeff11223344556677889900aabbccddeeff11223344556677889900",
        "foo",
        DirType::ComponentStorage
    )]
    #[test_case(
        "aabbccddeeff11223344556677889900aabbccddeeff11223344556677889900",
        "",
        DirType::ComponentStorage
    )]
    #[test_case("a:0", "", DirType::Component)]
    #[test_case("a:0/data", "", DirType::ComponentStorage)]
    #[test_case("a:0/data", "foo/bar", DirType::ComponentStorage)]
    #[test_case("a:0/whatisthis", "", DirType::Unknown)]
    #[test_case("a:0/whatisthis", "other/stuff", DirType::Unknown)]
    #[test_case("a:0/children", "", DirType::Children)]
    #[test_case("a:0/children/z:0/data", "", DirType::ComponentStorage)]
    #[test_case("a:0/children/z:0/children/m:0/children/b:0/data", "", DirType::ComponentStorage)]
    #[test_case("a:0/children/z:0/children/m:0/children/b:0/children", "", DirType::Children)]
    #[test_case(
        "a:0/children/z:0/children/m:0/children/b:0/data/",
        "some/leftover/stuff",
        DirType::ComponentStorage
    )]
    #[fuchsia::test]
    fn test_path_identification(path: &str, remainder: &str, r#type: DirType) {
        let data_path = PathBuf::from(path);
        let path_remainder = PathBuf::from(remainder);
        let full_path = data_path.join(&path_remainder);

        assert_eq!(StorageAdmin::is_storage_dir(full_path), (r#type, data_path, path_remainder));
    }

    async fn create_file(directory: &fio::DirectoryProxy, file_name: &str, contents: &str) {
        let file = fuchsia_fs::directory::open_file(
            directory,
            file_name,
            fio::Flags::FLAG_MUST_CREATE | fio::PERM_READABLE | fio::PERM_WRITABLE,
        )
        .await
        .expect("Failed to create file");
        fuchsia_fs::file::write(&file, contents).await.expect("Failed to write file contents");
    }

    async fn create_directory(
        directory: &fio::DirectoryProxy,
        directory_name: &str,
    ) -> fio::DirectoryProxy {
        fuchsia_fs::directory::create_directory(
            directory,
            directory_name,
            fio::PERM_READABLE | fio::PERM_WRITABLE,
        )
        .await
        .expect("Failed to create directory")
    }

    fn open_tempdir(tempdir: &tempfile::TempDir) -> fio::DirectoryProxy {
        fuchsia_fs::directory::open_in_namespace(
            tempdir.path().to_str().unwrap(),
            fio::PERM_READABLE | fio::PERM_WRITABLE,
        )
        .expect("Failed to open directory")
    }

    const NO_DIR_ENTRIES: &[String] = &[];

    /// Returns a sorted list of directory entry names.
    async fn readdir(directory: &fio::DirectoryProxy) -> Vec<String> {
        let mut entries: Vec<String> = fuchsia_fs::directory::readdir(directory)
            .await
            .expect("Failed to read directory")
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        entries.sort();
        entries
    }

    async fn delete_all_storage(storage: &fio::DirectoryProxy) -> Result<(), StorageError> {
        StorageAdmin::delete_all_storage(&storage, StorageAdmin::delete_dir_contents).await
    }

    #[fuchsia::test]
    async fn test_id_storage_simple_no_files() {
        const COMPONENT_STORAGE_DIR_NAME: &str =
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";

        let temp_directory = tempfile::tempdir().unwrap();
        let storage_dir = open_tempdir(&temp_directory);
        create_directory(&storage_dir, COMPONENT_STORAGE_DIR_NAME).await;

        delete_all_storage(&storage_dir).await.unwrap();
    }

    #[fuchsia::test]
    async fn test_id_storage_simple_files() {
        const COMPONENT_STORAGE_DIR_NAME: &str =
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";

        let temp_directory = tempfile::tempdir().unwrap();
        let storage_host = open_tempdir(&temp_directory);
        let component_storage = create_directory(&storage_host, COMPONENT_STORAGE_DIR_NAME).await;
        create_file(&component_storage, "file1.txt", "hello world").await;
        create_file(&component_storage, "file2", "hi there").await;

        delete_all_storage(&storage_host).await.unwrap();

        // Check that the component storage dir was emptied
        assert_eq!(readdir(&component_storage).await, NO_DIR_ENTRIES);

        // Check that the top-level storage directory still contains exactly
        // one item and that it's name is the expected one
        assert_eq!(readdir(&storage_host).await, [COMPONENT_STORAGE_DIR_NAME]);
    }

    #[fuchsia::test]
    /// The directory structure should look something like
    ///   abcdef1234567890abcdef1234567890/
    ///      something.png
    ///      subdir1/
    ///             file
    ///             subdir2/
    ///                     file.txt

    async fn test_id_storage_nested_contents_deleted() {
        const COMPONENT_STORAGE_DIR_NAME: &str =
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";

        let temp_directory = tempfile::tempdir().unwrap();
        let storage_host = open_tempdir(&temp_directory);
        let component_storage = create_directory(&storage_host, COMPONENT_STORAGE_DIR_NAME).await;

        let subdir = create_directory(&component_storage, "subdir1").await;
        create_file(&component_storage, "something.png", "not really a picture").await;

        let nested_subdir = create_directory(&subdir, "subdir2").await;
        create_file(&subdir, "file", "so we meet again!").await;

        create_file(&nested_subdir, "file.text", "hola").await;

        delete_all_storage(&storage_host).await.unwrap();

        // Check that the top-level storage directory still contains exactly
        // one item and that it's name is the expected one
        assert_eq!(readdir(&storage_host).await, [COMPONENT_STORAGE_DIR_NAME]);
        assert_eq!(readdir(&component_storage).await, NO_DIR_ENTRIES);
        assert_eq!(readdir(&subdir).await, NO_DIR_ENTRIES);
        assert_eq!(readdir(&nested_subdir).await, NO_DIR_ENTRIES);
    }

    #[fuchsia::test]
    /// Directory structure should look about like
    ///   /
    ///    a:0/data/file.txt
    ///    component/data/file.txt

    async fn test_moniker_storage_simple() {
        let temp_directory = tempfile::tempdir().unwrap();
        let storage_host = open_tempdir(&temp_directory);

        const COMPONENT1_DIR_NAME: &str = "a:0";
        let component1_dir = create_directory(&storage_host, COMPONENT1_DIR_NAME).await;
        let storage_data_dir1 = create_directory(&component1_dir, "data").await;
        create_file(&storage_data_dir1, "file.txt", "hello world!").await;

        // Although most monikers have a colon-number postfix, there's nothing
        // that requires this so let's try something without it
        const COMPONENT2_DIR_NAME: &str = "component";
        let component2_dir = create_directory(&storage_host, COMPONENT2_DIR_NAME).await;
        let storage_data_dir2 = create_directory(&component2_dir, "data").await;
        create_file(&storage_data_dir2, "file.txt", "hello yourself!").await;

        delete_all_storage(&storage_host).await.unwrap();

        // Verify the components' storage directories are still present
        assert_eq!(readdir(&storage_host).await, [COMPONENT1_DIR_NAME, COMPONENT2_DIR_NAME]);

        // Verify the directories still have a "data" subdirectory
        assert_eq!(readdir(&component1_dir).await, ["data"]);
        assert_eq!(readdir(&component2_dir).await, ["data"]);

        // Verify the data subdirs were purged
        assert_eq!(readdir(&storage_data_dir1).await, NO_DIR_ENTRIES);
        assert_eq!(readdir(&storage_data_dir2).await, NO_DIR_ENTRIES);
    }

    #[fuchsia::test]
    /// The directory structure looks like
    /// a:0/data/
    ///          b_file
    ///          subdir/
    ///                 a_file
    async fn test_moniker_storage_nested_deletion() {
        const COMPONENT_DIR_NAME: &str = "a:0";

        let temp_directory = tempfile::tempdir().unwrap();
        let storage_host = open_tempdir(&temp_directory);
        let component_dir = create_directory(&storage_host, COMPONENT_DIR_NAME).await;
        let data_dir = create_directory(&component_dir, "data").await;
        create_file(&data_dir, "b_file", "other content").await;
        let subdir = create_directory(&data_dir, "subdir").await;
        create_file(&subdir, "a_file", "content").await;

        delete_all_storage(&storage_host).await.unwrap();

        assert_eq!(readdir(&storage_host).await, [COMPONENT_DIR_NAME]);
        assert_eq!(readdir(&component_dir).await, ["data"]);
        assert_eq!(readdir(&data_dir).await, NO_DIR_ENTRIES);
        assert_eq!(readdir(&subdir).await, NO_DIR_ENTRIES);
    }

    #[fuchsia::test]
    /// The directory structure looks something like
    /// a:0/children/b:0/data/
    ///                       file.txt
    ///                       file2.txt
    async fn test_moniker_storage_child_data_deletion() {
        const PARENT_DIR_NAME: &str = "a:0";
        const CHILD_DIR_NAME: &str = "b:0";

        let temp_directory = tempfile::tempdir().unwrap();
        let storage_host = open_tempdir(&temp_directory);
        let parent_dir = create_directory(&storage_host, PARENT_DIR_NAME).await;
        let children_dir = create_directory(&parent_dir, "children").await;
        let child_dir = create_directory(&children_dir, CHILD_DIR_NAME).await;
        let child_data_dir = create_directory(&child_dir, "data").await;
        create_file(&child_data_dir, "file1.txt", "hello").await;
        create_file(&child_data_dir, "file2.txt", " world!").await;

        delete_all_storage(&storage_host).await.unwrap();

        assert_eq!(readdir(&storage_host).await, [PARENT_DIR_NAME]);
        assert_eq!(readdir(&parent_dir).await, ["children"]);
        assert_eq!(readdir(&children_dir).await, [CHILD_DIR_NAME]);
        assert_eq!(readdir(&child_dir).await, ["data"]);
        assert_eq!(readdir(&child_data_dir).await, NO_DIR_ENTRIES);
    }

    #[fuchsia::test]
    /// The directory layout is
    /// a:0/children/
    ///              b:0/data/file1.txt
    ///              c:0/data/file2.txt
    async fn test_moniker_storage_multipled_nested_children_cleared() {
        const PARENT_DIR_NAME: &str = "a:0";
        const CHILD1_NAME: &str = "b:0";
        const CHILD2_NAME: &str = "c:0";

        let temp_directory = tempfile::tempdir().unwrap();
        let storage_host = open_tempdir(&temp_directory);
        let parent_dir = create_directory(&storage_host, PARENT_DIR_NAME).await;
        let children_dir = create_directory(&parent_dir, "children").await;

        let child1_dir = create_directory(&children_dir, CHILD1_NAME).await;
        let child1_data_dir = create_directory(&child1_dir, "data").await;
        create_file(&child1_data_dir, "file1.txt", "hello").await;

        let child2_dir = create_directory(&children_dir, CHILD2_NAME).await;
        let child2_data_dir = create_directory(&child2_dir, "data").await;
        create_file(&child2_data_dir, "file2.txt", " world!").await;

        delete_all_storage(&storage_host).await.unwrap();

        assert_eq!(readdir(&storage_host).await, [PARENT_DIR_NAME]);
        assert_eq!(readdir(&parent_dir).await, ["children"]);
        assert_eq!(readdir(&children_dir).await, [CHILD1_NAME, CHILD2_NAME]);
        assert_eq!(readdir(&child1_dir).await, ["data"]);
        assert_eq!(readdir(&child1_data_dir).await, NO_DIR_ENTRIES);
        assert_eq!(readdir(&child2_dir).await, ["data"]);
        assert_eq!(readdir(&child2_data_dir).await, NO_DIR_ENTRIES);
    }

    #[fuchsia::test]
    async fn test_moniker_storage_no_data_dirs() {
        const COMPONENT1_NAME: &str = "a:0";
        const COMPONENT2_NAME: &str = "b:0";

        let temp_directory = tempfile::tempdir().unwrap();
        let storage_host = open_tempdir(&temp_directory);
        create_directory(&storage_host, COMPONENT1_NAME).await;
        create_directory(&storage_host, COMPONENT2_NAME).await;

        match delete_all_storage(&storage_host).await {
            Err(StorageError::NoStorageFound) => {}
            v => panic!("Expected {:?}, found {:?}", StorageError::NoStorageFound, v),
        }
    }

    #[fuchsia::test]
    async fn test_deletion_fn_called() {
        const HEX_STORAGE_ID: &str =
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        const FILENAME_IGNORED: &str = "file";

        let temp_directory = tempfile::tempdir().unwrap();
        let test_dir = open_tempdir(&temp_directory);
        create_file(&test_dir, FILENAME_IGNORED, "hello world!").await;
        let storage_dir = create_directory(&test_dir, HEX_STORAGE_ID).await;
        create_file(&storage_dir, "component_data.txt", "{}").await;
        create_file(&storage_dir, "component_data_another", "fa la la da da te da").await;
        let nested_dir = create_directory(&storage_dir, "subdir").await;
        create_file(&nested_dir, "nested_file.txt", "hello world").await;

        delete_all_storage(&test_dir).await.unwrap();

        assert_eq!(readdir(&test_dir).await, [HEX_STORAGE_ID, FILENAME_IGNORED]);
        assert_eq!(readdir(&storage_dir).await, NO_DIR_ENTRIES);
        assert_eq!(readdir(&nested_dir).await, NO_DIR_ENTRIES);
    }

    struct FakeDir {
        used: u64,
        total: u64,
    }

    impl std::fmt::Debug for FakeDir {
        fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
            f.write_fmt(format_args!("used: {}; total: {}", self.used, self.total))
        }
    }

    impl vfs::directory::entry::GetEntryInfo for FakeDir {
        fn entry_info(&self) -> vfs::directory::entry::EntryInfo {
            vfs::directory::entry::EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        }
    }

    impl vfs::node::Node for FakeDir {
        async fn get_attributes(
            &self,
            _requested_attributes: fio::NodeAttributesQuery,
        ) -> Result<fio::NodeAttributes2, zx::Status> {
            Err(zx::Status::INTERNAL)
        }

        fn query_filesystem(&self) -> Result<fio::FilesystemInfo, zx::Status> {
            Ok(fio::FilesystemInfo {
                total_bytes: self.total.into(),
                used_bytes: self.used.into(),
                total_nodes: 0,
                used_nodes: 0,
                free_shared_pool_bytes: 0,
                fs_id: 0,
                block_size: 512,
                max_filename_size: 100,
                fs_type: 0,
                padding: 0,
                name: [0; 32],
            })
        }
    }

    impl Directory for FakeDir {
        fn open(
            self: Arc<Self>,
            scope: ExecutionScope,
            flags: fio::OpenFlags,
            _path: Path,
            server_end: ServerEnd<fio::NodeMarker>,
        ) {
            flags.to_object_request(server_end).handle(|object_request| {
                object_request.spawn_connection(scope, self, flags, ImmutableConnection::create)
            });
        }

        fn open3(
            self: Arc<Self>,
            scope: ExecutionScope,
            _path: Path,
            flags: fio::Flags,
            object_request: ObjectRequestRef<'_>,
        ) -> Result<(), zx::Status> {
            object_request.take().handle(|object_request| {
                object_request.spawn_connection(scope, self, flags, ImmutableConnection::create)
            });
            Ok(())
        }

        async fn read_dirents<'a>(
            &'a self,
            _pos: &'a TraversalPosition,
            _sink: Box<dyn dirents_sink::Sink>,
        ) -> Result<(TraversalPosition, Box<dyn dirents_sink::Sealed>), zx::Status> {
            Err(zx::Status::INTERNAL)
        }

        fn register_watcher(
            self: Arc<Self>,
            _scope: ExecutionScope,
            _mask: fio::WatchMask,
            _watcher: DirectoryWatcher,
        ) -> Result<(), zx::Status> {
            Err(zx::Status::INTERNAL)
        }

        fn unregister_watcher(self: Arc<Self>, _key: usize) {
            panic!("not implemented!");
        }
    }

    #[fuchsia::test]
    async fn test_get_storage_utilization() {
        let execution_scope = ExecutionScope::new();
        let (client, server) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();

        let used = 10;
        let total = 1000;
        let fake_dir = Arc::new(FakeDir { used, total });

        fake_dir.open(
            execution_scope.clone(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            Path::dot(),
            fidl::endpoints::ServerEnd::new(server.into_channel()),
        );

        let storage_admin = client.into_proxy();
        let status = StorageAdmin::get_storage_status(&storage_admin).await.unwrap();

        assert_eq!(status.used_size.unwrap(), used);
        assert_eq!(status.total_size.unwrap(), total);
    }
}
