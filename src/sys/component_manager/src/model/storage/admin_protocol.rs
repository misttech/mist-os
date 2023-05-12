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

use {
    crate::{
        capability::{CapabilityProvider, CapabilitySource, PERMITTED_FLAGS},
        model::{
            component::{ComponentInstance, WeakComponentInstance},
            error::{CapabilityProviderError, ModelError},
            hooks::{Event, EventPayload, EventType, Hook, HooksRegistration},
            model::Model,
            routing::{Route, RouteSource},
            storage::{self, BackingDirectoryInfo},
        },
    },
    ::routing::capability_source::ComponentCapability,
    anyhow::{format_err, Context, Error},
    async_trait::async_trait,
    cm_moniker::InstancedRelativeMoniker,
    cm_rust::{CapabilityName, ExposeDecl, OfferDecl, StorageDecl, UseDecl},
    cm_task_scope::TaskScope,
    cm_util::channel,
    fidl::{endpoints::ServerEnd, prelude::*},
    fidl_fuchsia_component as fcomponent,
    fidl_fuchsia_io::{self as fio, DirectoryProxy, DirentType},
    fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync,
    fuchsia_fs::directory as ffs_dir,
    fuchsia_zircon as zx,
    futures::{
        stream::{FuturesUnordered, StreamExt},
        Future, TryFutureExt, TryStreamExt,
    },
    lazy_static::lazy_static,
    moniker::{AbsoluteMonikerBase, RelativeMoniker, RelativeMonikerBase},
    routing::{
        component_id_index::ComponentInstanceId, component_instance::ComponentInstanceInterface,
        RouteRequest,
    },
    std::{
        convert::{From, TryFrom},
        path::PathBuf,
        str::FromStr,
        sync::{Arc, Weak},
    },
    tracing::{debug, error, warn},
};

lazy_static! {
    pub static ref STORAGE_ADMIN_PROTOCOL_NAME: CapabilityName =
        fsys::StorageAdminMarker::PROTOCOL_NAME.into();
}

struct StorageAdminProtocolProvider {
    storage_decl: StorageDecl,
    component: WeakComponentInstance,
    storage_admin: Arc<StorageAdmin>,
}

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
                Some(DeletionErrorCause::File(_)) => fsys::DeletionError::Protocol,
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
    File(i32),
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

impl StorageAdminProtocolProvider {
    /// # Arguments
    /// * `storage_decl`: The declaration in the defining `component`'s
    ///    manifest.
    /// * `component`: Reference to the component that defined the storage
    ///    capability.
    /// * `storage_admin`: An implementer of the StorageAdmin protocol. If this
    ///   StorageAdminProtocolProvider is opened, this will be used to actually
    ///   serve the StorageAdmin protocol.
    pub fn new(
        storage_decl: StorageDecl,
        component: WeakComponentInstance,
        storage_admin: Arc<StorageAdmin>,
    ) -> Self {
        Self { storage_decl, component, storage_admin }
    }
}

#[async_trait]
impl CapabilityProvider for StorageAdminProtocolProvider {
    async fn open(
        self: Box<Self>,
        task_scope: TaskScope,
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), CapabilityProviderError> {
        let forbidden = flags - PERMITTED_FLAGS;
        if !forbidden.is_empty() {
            warn!(?forbidden, "StorageAdmin protocol");
            return Err(CapabilityProviderError::BadFlags);
        }

        if relative_path.components().count() != 0 {
            warn!(
                path=%relative_path.display(),
                "StorageAdmin protocol got open request with non-empty",
            );
            return Err(CapabilityProviderError::BadPath);
        }

        let server_end = channel::take_channel(server_end);

        let storage_decl = self.storage_decl.clone();
        let component = self.component.clone();
        let storage_admin = self.storage_admin.clone();
        task_scope
            .add_task(async move {
                if let Err(error) = storage_admin.serve(storage_decl, component, server_end).await {
                    warn!(?error, "failed to serve storage admin protocol");
                }
            })
            .await;
        Ok(())
    }
}

pub struct StorageAdmin {
    model: Weak<Model>,
}

// `StorageAdmin` is a `Hook` that serves the `StorageAdmin` FIDL protocol.
impl StorageAdmin {
    pub fn new(model: Weak<Model>) -> Self {
        Self { model }
    }

    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![HooksRegistration::new(
            "StorageAdmin",
            vec![EventType::CapabilityRouted],
            Arc::downgrade(self) as Weak<dyn Hook>,
        )]
    }

    pub async fn extract_storage_decl(
        source_capability: &ComponentCapability,
        component: WeakComponentInstance,
    ) -> Result<Option<StorageDecl>, ModelError> {
        match source_capability {
            ComponentCapability::Offer(OfferDecl::Protocol(_))
            | ComponentCapability::Expose(ExposeDecl::Protocol(_))
            | ComponentCapability::Use(UseDecl::Protocol(_)) => (),
            _ => return Ok(None),
        }
        if source_capability.source_name() != Some(&fsys::StorageAdminMarker::PROTOCOL_NAME.into())
        {
            return Ok(None);
        }
        let source_capability_name = source_capability.source_capability_name();
        if source_capability_name.is_none() {
            return Ok(None);
        }
        let source_component = component.upgrade()?;
        let source_component_state = source_component.lock_resolved_state().await?;
        let decl = source_component_state.decl();
        Ok(decl.find_storage_source(source_capability_name.unwrap()).cloned())
    }

    /// If `capability_provider` is `None` this attempts to create a provider
    /// based on the declaration represented by `source_capability` evaluated
    /// in the context of `component`. If `source_capability` contains a valid
    /// capability declaration this function returns the provider, otherwise an
    /// error.
    ///
    /// # Arguments
    /// * `source_capability`: The capability that represents the storage.
    /// * `component`: The component that defined the storage capability.
    /// * `capability_provider`: The provider of the capability, if any.
    ///   Normally we expect this to be `None` because component_manager is
    ///   usually the provider.
    async fn on_scoped_framework_capability_routed_async<'a>(
        self: Arc<Self>,
        source_capability: &'a ComponentCapability,
        component: WeakComponentInstance,
        capability_provider: Option<Box<dyn CapabilityProvider>>,
    ) -> Result<Option<Box<dyn CapabilityProvider>>, ModelError> {
        // If some other capability has already been installed, then there's nothing to
        // do here.
        if capability_provider.is_some() {
            return Ok(capability_provider);
        }
        // Find the storage decl, if it exists we're good to go
        let storage_decl = Self::extract_storage_decl(source_capability, component.clone()).await?;
        if let Some(storage_decl) = storage_decl {
            return Ok(Some(Box::new(StorageAdminProtocolProvider::new(
                storage_decl,
                component,
                self.clone(),
            )) as Box<dyn CapabilityProvider>));
        }
        // The declaration referenced either a nonexistent capability, or a capability that isn't a
        // storage capability. We can't be the provider for this.
        Ok(None)
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
        server_end: zx::Channel,
    ) -> Result<(), Error> {
        let storage_source = RouteSource {
            source: CapabilitySource::Component {
                capability: ComponentCapability::Storage(storage_decl.clone()),
                component: component.clone(),
            },
            relative_path: PathBuf::new(),
        };
        let backing_dir_source_info = storage::route_backing_directory(storage_source.source)
            .await
            .context("could not serve storage protocol, routing backing directory failed")?;

        let component = component.upgrade().map_err(|e| {
            format_err!(
                "unable to serve storage admin protocol, model reference is no longer valid: {:?}",
                e,
            )
        })?;

        let mut stream = ServerEnd::<fsys::StorageAdminMarker>::new(server_end)
            .into_stream()
            .expect("could not convert channel into stream");

        while let Some(request) = stream.try_next().await? {
            match request {
                fsys::StorageAdminRequest::OpenComponentStorage {
                    relative_moniker,
                    flags,
                    mode,
                    object,
                    control_handle: _,
                } => {
                    let instanced_relative_moniker =
                        InstancedRelativeMoniker::try_from(relative_moniker.as_str())?;
                    let abs_moniker = component
                        .abs_moniker()
                        .descendant(&instanced_relative_moniker.without_instance_ids());
                    let instance_id =
                        component.component_id_index().look_up_moniker(&abs_moniker).cloned();

                    let dir_proxy = storage::open_isolated_storage(
                        &backing_dir_source_info,
                        component.persistent_storage,
                        instanced_relative_moniker,
                        instance_id.as_ref(),
                    )
                    .await?;
                    dir_proxy.open(flags, mode, ".", object)?;
                }
                fsys::StorageAdminRequest::ListStorageInRealm {
                    relative_moniker,
                    iterator,
                    responder,
                } => {
                    let fut = async {
                        let model = self.model.upgrade().ok_or(fcomponent::Error::Internal)?;
                        let relative_moniker = RelativeMoniker::parse_str(&relative_moniker)
                            .map_err(|_| fcomponent::Error::InvalidArguments)?;
                        let absolute_moniker = component.abs_moniker.descendant(&relative_moniker);
                        let root_component = model
                            .look_up(&absolute_moniker)
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
                            responder.send(&mut Ok(()))?;
                        }
                        Err(e) => {
                            responder.send(&mut Err(e))?;
                        }
                    }
                }
                fsys::StorageAdminRequest::OpenComponentStorageById { id, object, responder } => {
                    let instance_id_index = component.component_id_index();
                    let component_id = match ComponentInstanceId::from_str(&id) {
                        Ok(id) => id,
                        Err(_) => {
                            responder.send(&mut Err(fcomponent::Error::InvalidArguments))?;
                            continue;
                        }
                    };
                    if !instance_id_index.look_up_instance_id(&component_id) {
                        responder.send(&mut Err(fcomponent::Error::ResourceNotFound))?;
                        continue;
                    }
                    match storage::open_isolated_storage_by_id(
                        &backing_dir_source_info,
                        component_id,
                    )
                    .await
                    {
                        Ok(dir) => responder.send(
                            &mut dir
                                .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, object)
                                .map_err(|_| fcomponent::Error::Internal),
                        )?,
                        Err(_) => responder.send(&mut Err(fcomponent::Error::Internal))?,
                    }
                }
                fsys::StorageAdminRequest::DeleteComponentStorage {
                    relative_moniker,
                    responder,
                } => {
                    let mut response = match InstancedRelativeMoniker::try_from(
                        relative_moniker.as_str(),
                    ) {
                        Err(error) => {
                            warn!(?error, "couldn't parse string as relative moniker for storage admin protocol");
                            Err(fcomponent::Error::InvalidArguments)
                        }
                        Ok(instanced_relative_moniker) => {
                            let abs_moniker = component
                                .abs_moniker()
                                .descendant(&instanced_relative_moniker.without_instance_ids());
                            let instance_id = component
                                .component_id_index()
                                .look_up_moniker(&abs_moniker)
                                .cloned();
                            let res = storage::delete_isolated_storage(
                                backing_dir_source_info.clone(),
                                component.persistent_storage,
                                instanced_relative_moniker,
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
                    responder.send(&mut response)?
                }
                fsys::StorageAdminRequest::GetStatus { responder } => {
                    if let Ok(storage_root) =
                        storage::open_storage_root(&backing_dir_source_info).await
                    {
                        responder.send_no_shutdown_on_err(
                            &mut Self::get_storage_status(&storage_root)
                                .await
                                .map_err(|e| e.into()),
                        )?;
                    } else {
                        responder.send_no_shutdown_on_err(&mut Err(fsys::StatusError::Provider))?;
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
                            Ok(_) => responder.send(&mut Ok(()))?,
                            Err(e) => {
                                warn!("errors encountered deleting storage: {:?}", e);
                                responder.send_no_shutdown_on_err(&mut Result::Err(e.into()))?;
                            }
                        }
                    } else {
                        // This might not be _entirely_ accurate, but in this error case we weren't
                        // able to talk to the directory, so that is, in a sense, lack of
                        // connection.
                        responder.send_no_shutdown_on_err(&mut Result::Err(
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
                        fuchsia_fs::OpenFlags::RIGHT_READABLE
                            | fuchsia_fs::OpenFlags::RIGHT_WRITABLE
                            | fuchsia_fs::OpenFlags::DIRECTORY,
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
                        Ok(Err(e)) => errors.push(DeletionErrorCause::File(e)),
                        _ => {}
                    }
                }
                ffs_dir::DirentKind::BlockDevice
                | ffs_dir::DirentKind::Service
                | ffs_dir::DirentKind::Unknown => {}
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
                    match storage::route_backing_directory(storage_source.source).await {
                        Ok(s) => s,
                        Err(_) => continue,
                    };

                if backing_dir_info == storage_capability_source_info {
                    let relative_moniker = InstancedRelativeMoniker::scope_down(
                        &backing_dir_info.storage_source_moniker,
                        &component.instanced_moniker(),
                    )
                    .unwrap();
                    storage_users.push(relative_moniker);
                    break;
                }
            }
            for component in component_state.children().map(|(_, v)| v) {
                components_to_visit.push(component.clone())
            }
        }

        const MAX_MONIKERS_RETURNED: usize = 10;
        let mut iterator_stream = iterator.into_stream()?;
        // TODO(fxbug.dev/77077): This currently returns monikers with instance ids, even though
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

#[async_trait]
impl Hook for StorageAdmin {
    async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
        match &event.payload {
            EventPayload::CapabilityRouted {
                source: CapabilitySource::Capability { source_capability, component },
                capability_provider,
            } => {
                let mut capability_provider = capability_provider.lock().await;
                *capability_provider = self
                    .on_scoped_framework_capability_routed_async(
                        source_capability,
                        component.clone(),
                        capability_provider.take(),
                    )
                    .await?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{DirType, StorageAdmin, StorageError},
        async_trait::async_trait,
        fidl::endpoints::{self, ServerEnd},
        fidl_fuchsia_io::{self as fio, DirectoryProxy},
        fuchsia_fs as ffs, fuchsia_zircon as zx,
        std::{fmt::Formatter, path::PathBuf, sync::Arc},
        test_case::test_case,
        vfs::{
            directory::{
                dirents_sink,
                entry::{DirectoryEntry, EntryInfo},
                entry_container::{Directory, DirectoryWatcher},
                helper::DirectlyMutable,
                immutable::connection::io1::ImmutableConnection,
                mutable::connection::io1::MutableConnection,
                simple::Simple,
                traversal_position::TraversalPosition,
            },
            execution_scope::ExecutionScope,
            file::vmo::read_only,
            mut_pseudo_directory,
            path::Path,
            ProtocolsExt, ToObjectRequest,
        },
    };

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

    fn connect_to_directory(
        dir: Arc<Simple<MutableConnection>>,
        scope: ExecutionScope,
    ) -> fio::DirectoryProxy {
        let (client, server) = endpoints::create_endpoints::<fidl_fuchsia_io::DirectoryMarker>();

        dir.open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            Path::dot(),
            fidl::endpoints::ServerEnd::new(server.into_channel()),
        );
        client.into_proxy().unwrap()
    }

    async fn delete_all_storage(storage: &DirectoryProxy) -> Result<(), StorageError> {
        StorageAdmin::delete_all_storage(storage, StorageAdmin::delete_dir_contents).await
    }

    #[fuchsia::test]
    async fn test_id_storage_simple_no_files() {
        let component_storage_dir_name =
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let component_storage_dir = mut_pseudo_directory! {};
        let storage_dir = mut_pseudo_directory! {
            component_storage_dir_name  => component_storage_dir,
        };

        let scope = ExecutionScope::new();
        let dir_proxy = connect_to_directory(storage_dir.clone(), scope.clone());

        delete_all_storage(&dir_proxy).await.unwrap();
    }

    #[fuchsia::test]
    async fn test_id_storage_simple_files() {
        let component_storage_dir_name =
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let component_storage_dir = mut_pseudo_directory! {
            "file1.txt" => read_only(b"hello world"),
            "file2" => read_only(b"hi there!"),
        };
        let storage_host = mut_pseudo_directory! {
            component_storage_dir_name => component_storage_dir.clone(),
        };

        let scope = ExecutionScope::new();
        let storage_host_proxy = connect_to_directory(storage_host.clone(), scope.clone());

        delete_all_storage(&storage_host_proxy).await.unwrap();

        // Check that the component storage dir was emptied
        let storage_dir_proxy = connect_to_directory(component_storage_dir.clone(), scope.clone());
        assert_eq!(0, ffs::directory::readdir(&storage_dir_proxy).await.unwrap().len());

        // Check that the top-level storage directory still contains exactly
        // one item and that it's name is the expected one
        let mut entries = ffs::directory::readdir(&storage_host_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!(component_storage_dir_name, &entries.pop().unwrap().name);
    }

    #[fuchsia::test]
    /// The directory structure should look something like
    ///   abcdef1234567890abcdef1234567890/
    ///      file.txt
    ///      subdir/
    ///             file.txt
    ///             subdir2/
    ///                     file.txt

    async fn test_id_storage_nested_contents_deleted() {
        let component_storage_dir_name =
            "12341234123412341234123412341234abcdef1234567890abcdef1234567890";
        let subdir_name = "subdir1";
        let nested_subdir_name = "subdir2";

        let nested_subdir = mut_pseudo_directory! {
            "file.text" => read_only(b"hola!")
        };

        let subdir = mut_pseudo_directory! {
            "file" => read_only(b"so we meet again!"),
            nested_subdir_name => nested_subdir.clone(),
        };

        let component_storage_dir = mut_pseudo_directory! {
            subdir_name => subdir.clone(),
            "something.png" => read_only(b"not really a picture"),
        };

        let storage_host = mut_pseudo_directory! {
            component_storage_dir_name => component_storage_dir.clone(),
        };

        let scope = ExecutionScope::new();
        let storage_host_proxy = connect_to_directory(storage_host.clone(), scope.clone());
        let storage_proxy = connect_to_directory(component_storage_dir.clone(), scope.clone());
        let subdir_proxy = connect_to_directory(subdir.clone(), scope.clone());
        let nested_subdir_proxy = connect_to_directory(subdir.clone(), scope.clone());

        delete_all_storage(&storage_host_proxy).await.unwrap();

        // Check that the top-level storage directory still contains exactly
        // one item and that it's name is the expected one
        let mut entries = ffs::directory::readdir(&storage_host_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!(component_storage_dir_name, &entries.pop().unwrap().name);

        assert_eq!(0, ffs::directory::readdir(&storage_proxy).await.unwrap().len());
        assert_eq!(0, ffs::directory::readdir(&subdir_proxy).await.unwrap().len());
        assert_eq!(0, ffs::directory::readdir(&nested_subdir_proxy).await.unwrap().len());
    }

    #[fuchsia::test]
    /// Directory structure should look about like
    ///   /
    ///    a:0/data/file.txt
    ///    component/data/file.txt

    async fn test_moniker_storage_simple() {
        let component1_dir_name = "a:0";
        // Although most monikers have a colon-number postfix, there's nothing
        // that requires this so let's try something without it
        let component2_dir_name = "component";

        let storage_data_dir1 = mut_pseudo_directory! {
            "file.txt" => read_only(b"hello world!"),
        };

        let component1_dir = mut_pseudo_directory! {
            "data" => storage_data_dir1.clone(),
        };

        let storage_data_dir2 = mut_pseudo_directory! {
            "file.txt" => read_only(b"hello yourself"),
        };

        let component2_dir = mut_pseudo_directory! {
            "data" => storage_data_dir2.clone(),
        };

        let storage_host_dir = mut_pseudo_directory! {
            component1_dir_name => component1_dir.clone(),
            component2_dir_name => component2_dir.clone(),
        };

        let scope = ExecutionScope::new();
        let component1_dir_proxy = connect_to_directory(component1_dir.clone(), scope.clone());
        let component2_dir_proxy = connect_to_directory(component2_dir.clone(), scope.clone());
        let data_dir1_proxy = connect_to_directory(storage_data_dir1.clone(), scope.clone());
        let data_dir2_proxy = connect_to_directory(storage_data_dir2.clone(), scope.clone());
        let storage_host_proxy = connect_to_directory(storage_host_dir.clone(), scope.clone());

        delete_all_storage(&storage_host_proxy).await.unwrap();

        // Verify the components' storage directories are still present
        let mut entries = ffs::directory::readdir(&storage_host_proxy).await.unwrap();
        assert_eq!(2, entries.len());
        assert_eq!(component2_dir_name, &entries.pop().unwrap().name);
        assert_eq!(component1_dir_name, &entries.pop().unwrap().name);

        // Verify the directories still have a "data" subdirectory
        entries = ffs::directory::readdir(&component1_dir_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!("data", entries.pop().unwrap().name);
        entries = ffs::directory::readdir(&component2_dir_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!("data", entries.pop().unwrap().name);

        // Verify the data subdirs were purged
        assert_eq!(0, ffs::directory::readdir(&data_dir1_proxy).await.unwrap().len());
        assert_eq!(0, ffs::directory::readdir(&data_dir2_proxy).await.unwrap().len());
    }

    #[fuchsia::test]
    /// The directory structure looks like
    /// a:0/data/
    ///          file.txt
    ///          subdir/
    ///                 file.txt
    async fn test_moniker_storage_nested_deletion() {
        let component_dir_name = "a:0";
        let subdir_name = "subdir";

        let subdir = mut_pseudo_directory! {
            "a_file" => read_only(b"content"),
        };

        let data_dir = mut_pseudo_directory! {
            "b_file" => read_only(b"other content"),
            subdir_name => subdir.clone(),
        };

        let component_dir = mut_pseudo_directory! {
            "data" => data_dir.clone(),
        };

        let storage_host_dir = mut_pseudo_directory! {
            component_dir_name => component_dir.clone(),
        };

        let scope = ExecutionScope::new();
        let subdir_proxy = connect_to_directory(subdir.clone(), scope.clone());
        let data_dir_proxy = connect_to_directory(data_dir.clone(), scope.clone());
        let component_dir_proxy = connect_to_directory(component_dir.clone(), scope.clone());
        let storage_host_proxy = connect_to_directory(storage_host_dir.clone(), scope.clone());

        delete_all_storage(&storage_host_proxy).await.unwrap();

        let mut entries = ffs::directory::readdir(&storage_host_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!(component_dir_name, &entries.pop().unwrap().name);

        entries = ffs::directory::readdir(&component_dir_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!("data", entries.pop().unwrap().name);

        assert_eq!(0, ffs::directory::readdir(&data_dir_proxy).await.unwrap().len());
        assert_eq!(0, ffs::directory::readdir(&subdir_proxy).await.unwrap().len());
    }

    #[fuchsia::test]
    /// The directory structure looks something like
    /// a:0/children/b:0/data/
    ///                       file.txt
    ///                       file2.txt
    async fn test_moniker_storage_child_data_deletion() {
        let parent_dir_name = "a:0";
        let child_dir_name = "b:0";

        let child_data_dir = mut_pseudo_directory! {
            "file1.txt" => read_only(b"hello"),
            "file2.txt" => read_only(b" world!"),
        };

        let child_dir = mut_pseudo_directory! {
            "data" => child_data_dir.clone(),
        };

        let children_dir = mut_pseudo_directory! {
            child_dir_name => child_dir.clone(),
        };

        let parent_dir = mut_pseudo_directory! {
            "children" => children_dir.clone(),
        };

        let storage_host_dir = mut_pseudo_directory! {
            parent_dir_name => parent_dir.clone(),
        };

        let scope = ExecutionScope::new();
        let child_data_dir_proxy = connect_to_directory(child_data_dir.clone(), scope.clone());
        let child_dir_proxy = connect_to_directory(child_dir.clone(), scope.clone());
        let children_dir_proxy = connect_to_directory(children_dir.clone(), scope.clone());
        let parent_dir_proxy = connect_to_directory(parent_dir.clone(), scope.clone());
        let storage_host_proxy = connect_to_directory(storage_host_dir.clone(), scope.clone());

        delete_all_storage(&storage_host_proxy).await.unwrap();

        let mut entries = ffs::directory::readdir(&storage_host_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!(parent_dir_name, entries.pop().unwrap().name);

        entries = ffs::directory::readdir(&parent_dir_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!("children", entries.pop().unwrap().name);

        entries = ffs::directory::readdir(&children_dir_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!(child_dir_name, entries.pop().unwrap().name);

        entries = ffs::directory::readdir(&child_dir_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!("data", entries.pop().unwrap().name);

        assert_eq!(0, ffs::directory::readdir(&child_data_dir_proxy).await.unwrap().len());
    }

    #[fuchsia::test]
    /// The directory layout is
    /// a:0/children/
    ///              b:0/data/file.txt
    ///              c:0/data/file.txt

    async fn test_moniker_storage_multipled_nested_children_cleared() {
        let parent_dir_name = "a:0";
        let child1_name = "b:0";
        let child2_name = "c:0";

        let child1_data_dir = mut_pseudo_directory! {
            "file1.txt" => read_only(b"hello"),
        };

        let child1_dir = mut_pseudo_directory! {
            "data" => child1_data_dir.clone(),
        };

        let child2_data_dir = mut_pseudo_directory! {
            "file2.txt" => read_only(b" world!"),
        };

        let child2_dir = mut_pseudo_directory! {
            "data" => child2_data_dir.clone(),
        };

        let children_dir = mut_pseudo_directory! {
            child1_name => child2_dir.clone(),
            child2_name => child1_dir.clone(),
        };

        let parent_dir = mut_pseudo_directory! {
            "children" => children_dir.clone(),
        };

        let storage_host_dir = mut_pseudo_directory! {
            parent_dir_name => parent_dir.clone(),
        };

        let scope = ExecutionScope::new();
        let child1_data_dir_proxy = connect_to_directory(child1_data_dir.clone(), scope.clone());
        let child1_dir_proxy = connect_to_directory(child1_dir.clone(), scope.clone());
        let child2_data_dir_proxy = connect_to_directory(child2_data_dir.clone(), scope.clone());
        let child2_dir_proxy = connect_to_directory(child2_dir.clone(), scope.clone());
        let children_dir_proxy = connect_to_directory(children_dir.clone(), scope.clone());
        let parent_dir_proxy = connect_to_directory(parent_dir.clone(), scope.clone());
        let storage_host_proxy = connect_to_directory(storage_host_dir.clone(), scope.clone());

        delete_all_storage(&storage_host_proxy).await.unwrap();

        let mut entries = ffs::directory::readdir(&storage_host_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!(parent_dir_name, entries.pop().unwrap().name);

        entries = ffs::directory::readdir(&parent_dir_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!("children", entries.pop().unwrap().name);

        entries = ffs::directory::readdir(&children_dir_proxy).await.unwrap();
        assert_eq!(2, entries.len());
        assert_eq!(child2_name, entries.pop().unwrap().name);
        assert_eq!(child1_name, entries.pop().unwrap().name);

        entries = ffs::directory::readdir(&child1_dir_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!("data", entries.pop().unwrap().name);

        assert_eq!(0, ffs::directory::readdir(&child1_data_dir_proxy).await.unwrap().len());

        entries = ffs::directory::readdir(&child2_dir_proxy).await.unwrap();
        assert_eq!(1, entries.len());
        assert_eq!("data", entries.pop().unwrap().name);

        assert_eq!(0, ffs::directory::readdir(&child2_data_dir_proxy).await.unwrap().len());
    }

    #[fuchsia::test]
    async fn test_moniker_storage_no_data_dirs() {
        let component1_name = "a:0";
        let component2_name = "b:0";

        let storage_host_dir = mut_pseudo_directory! {
            component1_name => mut_pseudo_directory!{},
            component2_name => mut_pseudo_directory!{},
        };

        let scope = ExecutionScope::new();
        let storage_host_proxy = connect_to_directory(storage_host_dir.clone(), scope.clone());

        match delete_all_storage(&storage_host_proxy).await {
            Err(StorageError::NoStorageFound) => {}
            v => panic!("Expected {:?}, found {:?}", StorageError::NoStorageFound, v),
        }
    }

    #[fuchsia::test]
    async fn test_deletion_fn_called() {
        let filename1 = "component_data.txt";
        let filename2 = "component_data_another";
        let filename3 = "nested_file.txt";

        let hex_storage_id = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let nested_dir_name = "subdir";

        let filename_ignored = "file";

        let nested_storage_dir = mut_pseudo_directory! {
            filename3 => read_only(b"hellow world"),
        };

        let storage_dir = mut_pseudo_directory! {
            filename1 => read_only(b"{}"),
            filename2 => read_only(b"fa la la da da te da"),
            nested_dir_name => nested_storage_dir.clone(),
        };

        storage_dir
            .add_entry("something", nested_storage_dir.clone())
            .expect("Adding entry failed");

        let test_dir = mut_pseudo_directory! {
            filename_ignored => read_only(b"hello world!"),
            hex_storage_id => storage_dir.clone(),
        };

        let (client, server) = endpoints::create_endpoints::<fidl_fuchsia_io::DirectoryMarker>();
        let dir_proxy = client.into_proxy().unwrap();

        let scope = ExecutionScope::new();
        test_dir.clone().open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            Path::dot(),
            fidl::endpoints::ServerEnd::new(server.into_channel()),
        );

        // Do some spot checks
        let _ = test_dir.get_entry(hex_storage_id).expect("entry retrieval failed!");
        let _ = storage_dir.get_entry(filename1).expect("subdir request failed.");

        // let mut deleted_paths = HashSet::<String>::new();

        StorageAdmin::delete_all_storage(&dir_proxy, StorageAdmin::delete_dir_contents)
            .await
            .unwrap();

        let _ = test_dir.get_entry(hex_storage_id).expect(
            "storage container directory retrieval failed, directory should not have been deleted",
        );
        let _ = test_dir
            .get_entry(filename_ignored)
            .expect("file at stop level appears deleted, but should have been retained.");
        match storage_dir.get_entry(filename1) {
            Ok(_) => panic!("entry unexpectedly present!"),
            Err(zx::Status::NOT_FOUND) => {}
            Err(e) => panic!("unexpected error checking for file presence: {:?}", e),
        }
        match storage_dir.get_entry(filename2) {
            Ok(_) => panic!("entry unexpectedly present!"),
            Err(zx::Status::NOT_FOUND) => {}
            Err(e) => panic!("unexpected error checking for file presence: {:?}", e),
        }
        match nested_storage_dir.get_entry(filename3) {
            Ok(_) => panic!("entry unexpectedly present!"),
            Err(zx::Status::NOT_FOUND) => {}
            Err(e) => panic!("unexpected error checking for file presence: {:?}", e),
        }
    }

    struct FakeDir {
        used: u64,
        total: u64,
        scope: ExecutionScope,
    }

    impl std::fmt::Debug for FakeDir {
        fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
            f.write_fmt(format_args!("used: {}; total: {}", self.used, self.total))
        }
    }

    impl DirectoryEntry for FakeDir {
        fn open(
            self: Arc<Self>,
            _scope: ExecutionScope,
            flags: fio::OpenFlags,
            _path: Path,
            server_end: ServerEnd<fio::NodeMarker>,
        ) {
            flags.to_object_request(server_end).handle(|object_request| {
                ImmutableConnection::create_connection(
                    self.scope.clone(),
                    self,
                    flags.to_directory_options()?,
                    object_request.take(),
                );
                Ok(())
            });
        }

        fn entry_info(&self) -> EntryInfo {
            panic!("not implemented!");
        }
    }

    #[async_trait]
    impl Directory for FakeDir {
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

        fn close(&self) -> Result<(), zx::Status> {
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
        async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
            Err(zx::Status::INTERNAL)
        }
    }

    #[fuchsia::test]
    async fn test_get_storage_utilization() {
        let execution_scope = ExecutionScope::new();
        let (client, server) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();

        let used = 10;
        let total = 1000;
        let fake_dir = Arc::new(FakeDir { used, total, scope: execution_scope.clone() });

        fake_dir.open(
            execution_scope.clone(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            Path::dot(),
            fidl::endpoints::ServerEnd::new(server.into_channel()),
        );

        let storage_admin = client.into_proxy().unwrap();
        let status = StorageAdmin::get_storage_status(&storage_admin).await.unwrap();

        assert_eq!(status.used_size.unwrap(), used);
        assert_eq!(status.total_size.unwrap(), total);
    }
}
