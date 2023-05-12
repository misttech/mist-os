// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Connection to a directory that can be modified by the client though a FIDL connection.

use crate::{
    common::{
        decode_extended_attribute_value, encode_extended_attribute_value,
        extended_attributes_sender,
    },
    directory::{
        connection::{
            io1::{BaseConnection, ConnectionState, DerivedConnection, WithShutdown as _},
            util::OpenDirectory,
        },
        entry::DirectoryEntry,
        entry_container::MutableDirectory,
        mutable::entry_constructor::NewEntryType,
        DirectoryOptions,
    },
    execution_scope::ExecutionScope,
    path::{validate_name, Path},
    token_registry::{TokenInterface, TokenRegistry, Tokenizable},
    ObjectRequest,
};

use {
    anyhow::Error,
    fidl::{endpoints::ServerEnd, Handle},
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    futures::{channel::oneshot, pin_mut, TryStreamExt as _},
    pin_project::pin_project,
    std::{pin::Pin, sync::Arc},
};

#[pin_project]
pub struct MutableConnection {
    base: BaseConnection<Self>,
}

impl DerivedConnection for MutableConnection {
    type Directory = dyn MutableDirectory;
    const MUTABLE: bool = true;

    fn new(
        scope: ExecutionScope,
        directory: OpenDirectory<Self::Directory>,
        options: DirectoryOptions,
    ) -> Self {
        MutableConnection { base: BaseConnection::<Self>::new(scope, directory, options) }
    }

    fn entry_not_found(
        scope: ExecutionScope,
        parent: Arc<dyn DirectoryEntry>,
        entry_type: NewEntryType,
        create: bool,
        name: &str,
        path: &Path,
    ) -> Result<Arc<dyn DirectoryEntry>, zx::Status> {
        match create {
            false => Err(zx::Status::NOT_FOUND),
            true => {
                let entry_constructor =
                    scope.entry_constructor().ok_or(zx::Status::NOT_SUPPORTED)?;
                entry_constructor.create_entry(parent, entry_type, name, path)
            }
        }
    }
}

impl MutableConnection {
    pub fn create_connection(
        scope: ExecutionScope,
        directory: Arc<dyn MutableDirectory>,
        options: DirectoryOptions,
        object_request: ObjectRequest,
    ) {
        // Ensure we close the directory if we fail to prepare the connection.
        let directory = OpenDirectory::new(directory);

        let connection = Self::new(scope.clone(), directory, options);

        // If we fail to send the task to the executor, it is probably shut down or is in the
        // process of shutting down (this is the only error state currently).  So there is
        // nothing for us to do - the connection will be closed automatically when the
        // connection object is dropped.
        let _ = scope.spawn_with_shutdown(move |shutdown| async {
            if let Ok(requests) = object_request.into_request_stream(&connection.base).await {
                Self::handle_requests(connection, requests, shutdown).await;
            }
        });
    }

    /// Very similar to create_connection, but creates a connection without spawning a new task.
    pub async fn create_connection_async(
        scope: ExecutionScope,
        directory: Arc<dyn MutableDirectory>,
        options: DirectoryOptions,
        object_request: ObjectRequest,
        shutdown: oneshot::Receiver<()>,
    ) {
        // Ensure we close the directory if we fail to prepare the connection.
        let directory = OpenDirectory::new(directory);

        let connection = Self::new(scope, directory, options);

        if let Ok(requests) = object_request.into_request_stream(&connection.base).await {
            Self::handle_requests(connection, requests, shutdown).await
        }
    }

    async fn handle_request(
        this: Pin<&mut Tokenizable<Self>>,
        request: fio::DirectoryRequest,
    ) -> Result<ConnectionState, Error> {
        match request {
            fio::DirectoryRequest::Unlink { name, options, responder } => {
                let result = this.as_mut().handle_unlink(name, options).await;
                responder.send(&mut result.map_err(zx::Status::into_raw))?;
            }
            fio::DirectoryRequest::GetToken { responder } => {
                let (status, token) = match Self::handle_get_token(this.into_ref()) {
                    Ok(token) => (zx::Status::OK, Some(token)),
                    Err(status) => (status, None),
                };
                responder.send(status.into_raw(), token)?;
            }
            fio::DirectoryRequest::Rename { src, dst_parent_token, dst, responder } => {
                let result = this.handle_rename(src, Handle::from(dst_parent_token), dst).await;
                responder.send(&mut result.map_err(zx::Status::into_raw))?;
            }
            fio::DirectoryRequest::SetAttr { flags, attributes, responder } => {
                let status = match this.as_mut().handle_setattr(flags, attributes).await {
                    Ok(()) => zx::Status::OK,
                    Err(status) => status,
                };
                responder.send(status.into_raw())?;
            }
            fio::DirectoryRequest::Sync { responder } => {
                responder
                    .send(&mut this.base.directory.sync().await.map_err(zx::Status::into_raw))?;
            }
            request @ (fio::DirectoryRequest::AddInotifyFilter { .. }
            | fio::DirectoryRequest::AdvisoryLock { .. }
            | fio::DirectoryRequest::Clone { .. }
            | fio::DirectoryRequest::Close { .. }
            | fio::DirectoryRequest::GetConnectionInfo { .. }
            | fio::DirectoryRequest::Enumerate { .. }
            | fio::DirectoryRequest::GetAttr { .. }
            | fio::DirectoryRequest::GetAttributes { .. }
            | fio::DirectoryRequest::GetFlags { .. }
            | fio::DirectoryRequest::Link { .. }
            | fio::DirectoryRequest::Open { .. }
            | fio::DirectoryRequest::Open2 { .. }
            | fio::DirectoryRequest::Query { .. }
            | fio::DirectoryRequest::QueryFilesystem { .. }
            | fio::DirectoryRequest::ReadDirents { .. }
            | fio::DirectoryRequest::Reopen { .. }
            | fio::DirectoryRequest::Rewind { .. }
            | fio::DirectoryRequest::SetFlags { .. }
            | fio::DirectoryRequest::UpdateAttributes { .. }
            | fio::DirectoryRequest::Watch { .. }) => {
                return this.as_mut().base.handle_request(request).await;
            }
            fio::DirectoryRequest::CreateSymlink {
                responder, name, target, connection, ..
            } => {
                if !this.base.options.rights.contains(fio::Operations::MODIFY_DIRECTORY) {
                    responder.send(&mut Err(zx::Status::ACCESS_DENIED.into_raw()))?;
                } else if !validate_name(&name) {
                    responder.send(&mut Err(zx::Status::INVALID_ARGS.into_raw()))?;
                } else {
                    responder.send(
                        &mut this
                            .as_mut()
                            .base
                            .directory
                            .create_symlink(name, target, connection)
                            .await
                            .map_err(|s| s.into_raw()),
                    )?;
                }
            }
            fio::DirectoryRequest::ListExtendedAttributes { iterator, control_handle: _ } => {
                fuchsia_trace::duration!("storage", "Directory::ListExtendedAttributes");
                this.handle_list_extended_attribute(iterator).await;
            }
            fio::DirectoryRequest::GetExtendedAttribute { name, responder } => {
                fuchsia_trace::duration!("storage", "Directory::GetExtendedAttribute");
                let mut res =
                    this.handle_get_extended_attribute(name).await.map_err(|s| s.into_raw());
                responder.send(&mut res)?;
            }
            fio::DirectoryRequest::SetExtendedAttribute { name, value, responder } => {
                fuchsia_trace::duration!("storage", "Directory::SetExtendedAttribute");
                let mut res =
                    this.handle_set_extended_attribute(name, value).await.map_err(|s| s.into_raw());
                responder.send(&mut res)?;
            }
            fio::DirectoryRequest::RemoveExtendedAttribute { name, responder } => {
                fuchsia_trace::duration!("storage", "Directory::RemoveExtendedAttribute");
                let mut res =
                    this.handle_remove_extended_attribute(name).await.map_err(|s| s.into_raw());
                responder.send(&mut res)?;
            }
        }
        Ok(ConnectionState::Alive)
    }

    async fn handle_setattr(
        self: Pin<&mut Self>,
        flags: fio::NodeAttributeFlags,
        attributes: fio::NodeAttributes,
    ) -> Result<(), zx::Status> {
        let DirectoryOptions { node, rights } = self.base.options;
        if node {
            // TODO(https://fxbug.dev/77623): should this be an error?
            // return Err(zx::Status::NOT_SUPPORTED);
        }
        if !rights.contains(fio::W_STAR_DIR) {
            return Err(zx::Status::BAD_HANDLE);
        }

        // TODO(jfsulliv): Consider always permitting attributes to be deferrable. The risk with
        // this is that filesystems would require a background flush of dirty attributes to disk.
        self.base.directory.set_attrs(flags, attributes).await
    }

    async fn handle_unlink(
        self: Pin<&mut Self>,
        name: String,
        options: fio::UnlinkOptions,
    ) -> Result<(), zx::Status> {
        let DirectoryOptions { node, rights } = self.base.options;
        if node {
            // TODO(https://fxbug.dev/77623): should this be an error?
            // return Err(zx::Status::NOT_SUPPORTED);
        }
        if !rights.contains(fio::W_STAR_DIR) {
            return Err(zx::Status::BAD_HANDLE);
        }

        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(zx::Status::INVALID_ARGS);
        }

        self.base
            .directory
            .clone()
            .unlink(
                &name,
                options
                    .flags
                    .map(|f| f.contains(fio::UnlinkFlags::MUST_BE_DIRECTORY))
                    .unwrap_or(false),
            )
            .await
    }

    fn handle_get_token(this: Pin<&Tokenizable<Self>>) -> Result<Handle, zx::Status> {
        let DirectoryOptions { node, rights } = this.base.options;
        if node {
            // TODO(https://fxbug.dev/77623): should this be an error?
            // return Err(zx::Status::NOT_SUPPORTED);
        }
        if !rights.contains(fio::W_STAR_DIR) {
            return Err(zx::Status::BAD_HANDLE);
        }
        Ok(TokenRegistry::get_token(this)?)
    }

    async fn handle_rename(
        &self,
        src: String,
        dst_parent_token: Handle,
        dst: String,
    ) -> Result<(), zx::Status> {
        let DirectoryOptions { node, rights } = self.base.options;
        if node {
            // TODO(https://fxbug.dev/77623): should this be an error?
            // return Err(zx::Status::NOT_SUPPORTED);
        }
        if !rights.contains(fio::W_STAR_DIR) {
            return Err(zx::Status::BAD_HANDLE);
        }

        let src = Path::validate_and_split(src)?;
        let dst = Path::validate_and_split(dst)?;

        if !src.is_single_component() || !dst.is_single_component() {
            return Err(zx::Status::INVALID_ARGS);
        }

        let (dst_parent, _flags) =
            match self.base.scope.token_registry().get_owner(dst_parent_token)? {
                None => return Err(zx::Status::NOT_FOUND),
                Some(entry) => entry,
            };

        dst_parent.clone().rename(self.base.directory.clone(), src, dst).await
    }

    async fn handle_list_extended_attribute(
        &self,
        iterator: ServerEnd<fio::ExtendedAttributeIteratorMarker>,
    ) {
        let attributes = match self.base.directory.list_extended_attributes().await {
            Ok(attributes) => attributes,
            Err(status) => {
                tracing::error!(?status, "list extended attributes failed");
                iterator
                    .close_with_epitaph(status)
                    .unwrap_or_else(|error| tracing::error!(?error, "failed to send epitaph"));
                return;
            }
        };
        self.base.scope.spawn(extended_attributes_sender(iterator, attributes));
    }

    async fn handle_get_extended_attribute(
        &self,
        name: Vec<u8>,
    ) -> Result<fio::ExtendedAttributeValue, zx::Status> {
        let value = self.base.directory.get_extended_attribute(name).await?;
        encode_extended_attribute_value(value)
    }

    async fn handle_set_extended_attribute(
        &self,
        name: Vec<u8>,
        value: fio::ExtendedAttributeValue,
    ) -> Result<(), zx::Status> {
        if name.iter().any(|c| *c == 0) {
            return Err(zx::Status::INVALID_ARGS);
        }
        let val = decode_extended_attribute_value(value)?;
        self.base.directory.set_extended_attribute(name, val).await
    }

    async fn handle_remove_extended_attribute(&self, name: Vec<u8>) -> Result<(), zx::Status> {
        self.base.directory.remove_extended_attribute(name).await
    }

    async fn handle_requests(
        self,
        requests: fio::DirectoryRequestStream,
        shutdown: oneshot::Receiver<()>,
    ) {
        let this = Tokenizable::new(self);
        pin_mut!(this);
        let mut requests = requests.with_shutdown(shutdown);
        while let Ok(Some(request)) = requests.try_next().await {
            if !matches!(
                Self::handle_request(Pin::as_mut(&mut this), request).await,
                Ok(ConnectionState::Alive)
            ) {
                break;
            }
        }
    }
}

impl TokenInterface for MutableConnection {
    fn get_node_and_flags(&self) -> (Arc<dyn MutableDirectory>, fio::OpenFlags) {
        (self.base.directory.clone(), self.base.options.to_io1())
    }

    fn token_registry(&self) -> &TokenRegistry {
        self.base.scope.token_registry()
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            directory::{
                dirents_sink,
                entry::{DirectoryEntry, EntryInfo},
                entry_container::{Directory, DirectoryWatcher},
                traversal_position::TraversalPosition,
            },
            path::Path,
            ProtocolsExt, ToObjectRequest,
        },
        async_trait::async_trait,
        fidl::endpoints::ServerEnd,
        std::{
            any::Any,
            sync::{Arc, Mutex, Weak},
        },
    };

    #[derive(Debug, PartialEq)]
    enum MutableDirectoryAction {
        Link { id: u32, path: String },
        Unlink { id: u32, name: String },
        Rename { id: u32, src_name: String, dst_dir: u32, dst_name: String },
        SetAttr { id: u32, flags: fio::NodeAttributeFlags, attrs: fio::NodeAttributes },
        Sync,
        Close,
    }

    #[derive(Debug)]
    struct MockDirectory {
        id: u32,
        fs: Arc<MockFilesystem>,
    }

    impl MockDirectory {
        pub fn new(id: u32, fs: Arc<MockFilesystem>) -> Arc<Self> {
            Arc::new(MockDirectory { id, fs })
        }
    }

    impl PartialEq for MockDirectory {
        fn eq(&self, other: &Self) -> bool {
            self.id == other.id
        }
    }

    impl DirectoryEntry for MockDirectory {
        fn open(
            self: Arc<Self>,
            _scope: ExecutionScope,
            _flags: fio::OpenFlags,
            _path: Path,
            _server_end: ServerEnd<fio::NodeMarker>,
        ) {
            panic!("Not implemented!");
        }

        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(0, fio::DirentType::Directory)
        }
    }

    #[async_trait]
    impl Directory for MockDirectory {
        async fn read_dirents<'a>(
            &'a self,
            _pos: &'a TraversalPosition,
            _sink: Box<dyn dirents_sink::Sink>,
        ) -> Result<(TraversalPosition, Box<dyn dirents_sink::Sealed>), zx::Status> {
            panic!("Not implemented");
        }

        fn register_watcher(
            self: Arc<Self>,
            _scope: ExecutionScope,
            _mask: fio::WatchMask,
            _watcher: DirectoryWatcher,
        ) -> Result<(), zx::Status> {
            panic!("Not implemented");
        }

        async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
            panic!("Not implemented");
        }

        fn unregister_watcher(self: Arc<Self>, _key: usize) {
            panic!("Not implemented");
        }

        fn close(&self) -> Result<(), zx::Status> {
            self.fs.handle_event(MutableDirectoryAction::Close)
        }
    }

    #[async_trait]
    impl MutableDirectory for MockDirectory {
        async fn link(
            self: Arc<Self>,
            path: String,
            _source_dir: Arc<dyn Any + Send + Sync>,
            _source_name: &str,
        ) -> Result<(), zx::Status> {
            self.fs.handle_event(MutableDirectoryAction::Link { id: self.id, path })
        }

        async fn unlink(
            self: Arc<Self>,
            name: &str,
            _must_be_directory: bool,
        ) -> Result<(), zx::Status> {
            self.fs.handle_event(MutableDirectoryAction::Unlink {
                id: self.id,
                name: name.to_string(),
            })
        }

        async fn set_attrs(
            &self,
            flags: fio::NodeAttributeFlags,
            attrs: fio::NodeAttributes,
        ) -> Result<(), zx::Status> {
            self.fs.handle_event(MutableDirectoryAction::SetAttr { id: self.id, flags, attrs })
        }

        async fn sync(&self) -> Result<(), zx::Status> {
            self.fs.handle_event(MutableDirectoryAction::Sync)
        }

        async fn rename(
            self: Arc<Self>,
            src_dir: Arc<dyn MutableDirectory>,
            src_name: Path,
            dst_name: Path,
        ) -> Result<(), zx::Status> {
            let src_dir = src_dir.into_any().downcast::<MockDirectory>().unwrap();
            self.fs.handle_event(MutableDirectoryAction::Rename {
                id: src_dir.id,
                src_name: src_name.into_string(),
                dst_dir: self.id,
                dst_name: dst_name.into_string(),
            })
        }
    }

    struct Events(Mutex<Vec<MutableDirectoryAction>>);

    impl Events {
        fn new() -> Arc<Self> {
            Arc::new(Events(Mutex::new(vec![])))
        }
    }

    struct MockFilesystem {
        cur_id: Mutex<u32>,
        scope: ExecutionScope,
        events: Weak<Events>,
    }

    impl MockFilesystem {
        pub fn new(events: &Arc<Events>) -> Self {
            let scope = ExecutionScope::new();
            MockFilesystem { cur_id: Mutex::new(0), scope, events: Arc::downgrade(events) }
        }

        pub fn handle_event(&self, event: MutableDirectoryAction) -> Result<(), zx::Status> {
            self.events.upgrade().map(|x| x.0.lock().unwrap().push(event));
            Ok(())
        }

        pub fn make_connection(
            self: &Arc<Self>,
            flags: fio::OpenFlags,
        ) -> (Arc<MockDirectory>, fio::DirectoryProxy) {
            let mut cur_id = self.cur_id.lock().unwrap();
            let dir = MockDirectory::new(*cur_id, self.clone());
            *cur_id += 1;
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            flags.to_object_request(server_end).handle(|object_request| {
                MutableConnection::create_connection(
                    self.scope.clone(),
                    dir.clone(),
                    flags.to_directory_options()?,
                    object_request.take(),
                );
                Ok(())
            });
            (dir, proxy)
        }
    }

    impl std::fmt::Debug for MockFilesystem {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MockFilesystem").field("cur_id", &self.cur_id).finish()
        }
    }

    #[fuchsia::test]
    async fn test_rename() {
        use zx::Event;

        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));

        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let (dir2, proxy2) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);

        let (status, token) = proxy2.get_token().await.unwrap();
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        let status = proxy.rename("src", Event::from(token.unwrap()), "dest").await.unwrap();
        assert!(status.is_ok());

        let events = events.0.lock().unwrap();
        assert_eq!(
            *events,
            vec![MutableDirectoryAction::Rename {
                id: 0,
                src_name: "src".to_owned(),
                dst_dir: dir2.id,
                dst_name: "dest".to_owned(),
            },]
        );
    }

    #[fuchsia::test]
    async fn test_setattr() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let attrs = fio::NodeAttributes {
            mode: 0,
            id: 0,
            content_size: 0,
            storage_size: 0,
            link_count: 0,
            creation_time: 30,
            modification_time: 100,
        };
        let status = proxy
            .set_attr(
                fio::NodeAttributeFlags::CREATION_TIME | fio::NodeAttributeFlags::MODIFICATION_TIME,
                &attrs,
            )
            .await
            .unwrap();
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        let events = events.0.lock().unwrap();
        assert_eq!(
            *events,
            vec![MutableDirectoryAction::SetAttr {
                id: 0,
                flags: fio::NodeAttributeFlags::CREATION_TIME
                    | fio::NodeAttributeFlags::MODIFICATION_TIME,
                attrs
            }]
        );
    }

    #[fuchsia::test]
    async fn test_link() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let (_dir2, proxy2) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);

        let (status, token) = proxy2.get_token().await.unwrap();
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);

        let status = proxy.link("src", token.unwrap(), "dest").await.unwrap();
        assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
        let events = events.0.lock().unwrap();
        assert_eq!(*events, vec![MutableDirectoryAction::Link { id: 1, path: "dest".to_owned() },]);
    }

    #[fuchsia::test]
    async fn test_unlink() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        proxy
            .unlink("test", &fio::UnlinkOptions::default())
            .await
            .expect("fidl call failed")
            .expect("unlink failed");
        let events = events.0.lock().unwrap();
        assert_eq!(
            *events,
            vec![MutableDirectoryAction::Unlink { id: 0, name: "test".to_string() },]
        );
    }

    #[fuchsia::test]
    async fn test_sync() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let () = proxy.sync().await.unwrap().map_err(zx::Status::from_raw).unwrap();
        let events = events.0.lock().unwrap();
        assert_eq!(*events, vec![MutableDirectoryAction::Sync]);
    }

    #[fuchsia::test]
    async fn test_close() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);
        let () = proxy.close().await.unwrap().map_err(zx::Status::from_raw).unwrap();
        let events = events.0.lock().unwrap();
        assert_eq!(*events, vec![MutableDirectoryAction::Close]);
    }

    #[fuchsia::test]
    async fn test_implicit_close() {
        let events = Events::new();
        let fs = Arc::new(MockFilesystem::new(&events));
        let (_dir, _proxy) = fs
            .clone()
            .make_connection(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE);

        fs.scope.shutdown();
        fs.scope.wait().await;

        let events = events.0.lock().unwrap();
        assert_eq!(*events, vec![MutableDirectoryAction::Close]);
    }
}
