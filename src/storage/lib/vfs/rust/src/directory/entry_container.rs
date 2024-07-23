// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! `EntryContainer` is a trait implemented by directories that allow manipulation of their
//! content.

use crate::directory::dirents_sink;
use crate::directory::traversal_position::TraversalPosition;
use crate::execution_scope::ExecutionScope;
use crate::node::Node;
use crate::object_request::ObjectRequestRef;
use crate::path::Path;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use fuchsia_zircon_status::Status;
use futures::future::BoxFuture;
use std::any::Any;
use std::future::{ready, Future};
use std::sync::Arc;

mod private {
    use fidl_fuchsia_io as fio;

    /// A type-preserving wrapper around [`fuchsia_async::Channel`].
    #[derive(Debug)]
    pub struct DirectoryWatcher {
        channel: fuchsia_async::Channel,
    }

    impl DirectoryWatcher {
        /// Provides access to the underlying channel.
        pub fn channel(&self) -> &fuchsia_async::Channel {
            let Self { channel } = self;
            channel
        }
    }

    impl TryFrom<fidl::endpoints::ServerEnd<fio::DirectoryWatcherMarker>> for DirectoryWatcher {
        type Error = fuchsia_zircon_status::Status;

        fn try_from(
            server_end: fidl::endpoints::ServerEnd<fio::DirectoryWatcherMarker>,
        ) -> Result<Self, Self::Error> {
            let channel = fuchsia_async::Channel::from_channel(server_end.into_channel());
            Ok(Self { channel })
        }
    }
}

pub use private::DirectoryWatcher;

/// All directories implement this trait.  If a directory can be modified it should
/// also implement the `MutableDirectory` trait.
pub trait Directory: Node {
    /// Opens a connection to this item if the `path` is "." or a connection to an item inside this
    /// one otherwise.  `path` will not contain any "." or ".." components.
    ///
    /// `flags` holds one or more of the `OPEN_RIGHT_*`, `OPEN_FLAG_*` constants.  Processing of the
    /// `flags` value is specific to the item - in particular, the `OPEN_RIGHT_*` flags need to
    /// match the item capabilities.
    ///
    /// It is the responsibility of the implementation to strip POSIX flags if the path crosses
    /// a boundary that does not have the required permissions.
    ///
    /// It is the responsibility of the implementation to send an `OnOpen` event on the channel
    /// contained by `server_end` in case `OPEN_FLAG_STATUS` was present in `flags`, and to
    /// populate the `info` part of the event if `OPEN_FLAG_DESCRIBE` was set.  This also applies
    /// to the error cases.
    ///
    /// This method is called via either `Open` or `Clone` fuchsia.io methods.  This is deliberate
    /// that this method does not return any errors.  Any errors that occur during this process
    /// should be sent as an `OnOpen` event over the `server_end` connection and the connection is
    /// then closed.  No errors should ever affect the connection where `Open` or `Clone` were
    /// received.
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    );

    /// Opens a connection to this item if the `path` is "." or a connection to an item inside
    /// this one otherwise.  `path` will not contain any "." or ".." components.
    ///
    /// `protocols` holds representations accepted by the caller, for example, it holds `node` that
    /// is the underlying `Node` protocol to be served on the connection. `node` holds information
    /// like the open mode (`mode`), node protocols (`protocols`), rights (`rights`) and create
    /// attributes (`create_attributes`).
    ///
    /// If this method was initiated by a FIDL Open2 call, hierarchical rights are enforced at the
    /// connection layer. The connection layer also checks that when creating a new object,
    /// no more than one protocol is specified and `create_attributes` is some value.
    ///
    /// If the implementation takes `object_request`, it is then responsible for sending an
    /// `OnRepresentation` event if `protocols` has `NodeFlags.GET_REPRESENTATION` set. Although
    /// not enforced, the implementation should shutdown with an epitaph if any error occurred
    /// during this process.
    ///
    /// See fuchsia.io's Open2 method for more details.
    fn open2(
        self: Arc<Self>,
        _scope: ExecutionScope,
        _path: Path,
        _protocols: fio::ConnectionProtocols,
        _object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    #[cfg(fuchsia_api_level_at_least = "HEAD")]
    fn open3(
        self: Arc<Self>,
        _scope: ExecutionScope,
        _path: Path,
        _flags: fio::Flags,
        _object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    /// Reads directory entries starting from `pos` by adding them to `sink`.
    /// Once finished, should return a sealed sink.
    // The lifetimes here are because of https://github.com/rust-lang/rust/issues/63033.
    fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        sink: Box<dyn dirents_sink::Sink>,
    ) -> impl Future<Output = Result<(TraversalPosition, Box<dyn dirents_sink::Sealed>), Status>> + Send
    where
        Self: Sized;

    /// Register a watcher for this directory.
    /// Implementations will probably want to use a `Watcher` to manage watchers.
    fn register_watcher(
        self: Arc<Self>,
        scope: ExecutionScope,
        mask: fio::WatchMask,
        watcher: DirectoryWatcher,
    ) -> Result<(), Status>;

    /// Unregister a watcher from this directory. The watcher should no longer
    /// receive events.
    fn unregister_watcher(self: Arc<Self>, key: usize);
}

/// This trait indicates a directory that can be mutated by adding and removing entries.
/// This trait must be implemented to use a `MutableConnection`, however, a directory could also
/// implement the `DirectlyMutable` type, which provides a blanket implementation of this trait.
pub trait MutableDirectory: Directory + Send + Sync {
    /// Adds a child entry to this directory.  If the target exists, it should fail with
    /// ZX_ERR_ALREADY_EXISTS.
    fn link<'a>(
        self: Arc<Self>,
        _name: String,
        _source_dir: Arc<dyn Any + Send + Sync>,
        _source_name: &'a str,
    ) -> BoxFuture<'a, Result<(), Status>> {
        Box::pin(ready(Err(Status::NOT_SUPPORTED)))
    }

    /// Set the attributes of this directory based on the values in `attrs`.
    fn set_attrs(
        &self,
        flags: fio::NodeAttributeFlags,
        attributes: fio::NodeAttributes,
    ) -> impl Future<Output = Result<(), Status>> + Send
    where
        Self: Sized;

    /// Set the mutable attributes of this directory based on the values in `attributes`.
    fn update_attributes(
        &self,
        attributes: fio::MutableNodeAttributes,
    ) -> impl Future<Output = Result<(), Status>> + Send
    where
        Self: Sized;

    /// Removes an entry from this directory.
    fn unlink(
        self: Arc<Self>,
        name: &str,
        must_be_directory: bool,
    ) -> impl Future<Output = Result<(), Status>> + Send
    where
        Self: Sized;

    /// Syncs the directory.
    fn sync(&self) -> impl Future<Output = Result<(), Status>> + Send
    where
        Self: Sized;

    /// Renames into this directory.
    fn rename(
        self: Arc<Self>,
        _src_dir: Arc<dyn MutableDirectory>,
        _src_name: Path,
        _dst_name: Path,
    ) -> BoxFuture<'static, Result<(), Status>> {
        Box::pin(ready(Err(Status::NOT_SUPPORTED)))
    }

    /// Creates a symbolic link.
    fn create_symlink(
        &self,
        _name: String,
        _target: Vec<u8>,
        _connection: Option<ServerEnd<fio::SymlinkMarker>>,
    ) -> impl Future<Output = Result<(), Status>> + Send
    where
        Self: Sized,
    {
        ready(Err(Status::NOT_SUPPORTED))
    }

    /// List extended attributes.
    fn list_extended_attributes(&self) -> impl Future<Output = Result<Vec<Vec<u8>>, Status>> + Send
    where
        Self: Sized,
    {
        ready(Err(Status::NOT_SUPPORTED))
    }

    /// Get the value for an extended attribute.
    fn get_extended_attribute(
        &self,
        _name: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Status>> + Send
    where
        Self: Sized,
    {
        ready(Err(Status::NOT_SUPPORTED))
    }

    /// Set the value for an extended attribute.
    fn set_extended_attribute(
        &self,
        _name: Vec<u8>,
        _value: Vec<u8>,
        _mode: fio::SetExtendedAttributeMode,
    ) -> impl Future<Output = Result<(), Status>> + Send
    where
        Self: Sized,
    {
        ready(Err(Status::NOT_SUPPORTED))
    }

    /// Remove the value for an extended attribute.
    fn remove_extended_attribute(
        &self,
        _name: Vec<u8>,
    ) -> impl Future<Output = Result<(), Status>> + Send
    where
        Self: Sized,
    {
        ready(Err(Status::NOT_SUPPORTED))
    }
}
