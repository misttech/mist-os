// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Module holding different kinds of files and their building blocks.

use crate::execution_scope::ExecutionScope;
use crate::node::Node;
use crate::object_request::ObjectRequestRef;
use crate::protocols::ProtocolsExt;
use fidl_fuchsia_io as fio;
use std::future::{ready, Future};
use std::sync::Arc;
use zx_status::Status;

/// File nodes backed by VMOs.
#[cfg(target_os = "fuchsia")]
pub mod vmo;

#[cfg(not(target_os = "fuchsia"))]
pub mod simple;

pub mod test_utils;

mod common;

pub mod connection;

pub use connection::{FidlIoConnection, RawIoConnection};

#[cfg(target_os = "fuchsia")]
pub use connection::{GetVmo, StreamIoConnection};

/// Creates a new read-only `SimpleFile` with the specified `content`.
///
/// ## Examples
/// ```
/// // Using static data:
/// let from_str = read_only("str");
/// let from_bytes = read_only(b"bytes");
/// // Using owned data:
/// let from_string = read_only(String::from("owned"));
/// let from_vec = read_only(vec![0u8; 2]);
/// ```
#[cfg(not(target_os = "fuchsia"))]
pub fn read_only(content: impl AsRef<[u8]>) -> Arc<simple::SimpleFile> {
    simple::SimpleFile::read_only(content)
}

#[cfg(target_os = "fuchsia")]
pub use vmo::read_only;

/// FileOptions include options that are relevant after the file has been opened. Flags like
/// `TRUNCATE`, which only applies during open time, are not included.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FileOptions {
    pub rights: fio::Operations,
    pub is_append: bool,
}

impl FileOptions {
    /// Converts to `StreamOptions`.
    #[cfg(target_os = "fuchsia")]
    pub fn to_stream_options(&self) -> zx::StreamOptions {
        let mut options = zx::StreamOptions::empty();
        if self.rights.contains(fio::Operations::READ_BYTES) {
            options |= zx::StreamOptions::MODE_READ;
        }
        if self.rights.contains(fio::Operations::WRITE_BYTES) {
            options |= zx::StreamOptions::MODE_WRITE;
        }
        if self.is_append {
            options |= zx::StreamOptions::MODE_APPEND;
        }
        options
    }

    pub(crate) fn to_io1(&self) -> fio::OpenFlags {
        let mut flags = fio::OpenFlags::empty();
        if self.rights.contains(fio::Operations::READ_BYTES) {
            flags |= fio::OpenFlags::RIGHT_READABLE;
        }
        if self.rights.contains(fio::Operations::WRITE_BYTES) {
            flags |= fio::OpenFlags::RIGHT_WRITABLE;
        }
        if self.rights.contains(fio::Operations::EXECUTE) {
            flags |= fio::OpenFlags::RIGHT_EXECUTABLE;
        }
        if self.is_append {
            flags |= fio::OpenFlags::APPEND;
        }
        flags
    }
}

#[derive(Default, PartialEq)]
pub enum SyncMode {
    /// Used when the Sync fuchsia.io method is used.
    #[default]
    Normal,

    /// Used when the connection is about to be closed. Typically this will involve flushing data
    /// from caches, but performance is a consideration, so it should only perform what might be
    /// necessary for closing the file. If anything *must* happen when a file is closed, it must be
    /// implemented in the `Node::close` function, not here; a call to sync with this mode is not
    /// guaranteed and not implementing/supporting it should have no effect on correctness. If
    /// `Node::close` needs to flush data in an async context, it has to spawn a task.  Supporting
    /// this mode means that in most cases there's no need to spawn a task because there should be
    /// nothing that needs to be flushed (but it must check). This will only be called if the
    /// connection has write permissions; a connection that only has read permissions should not
    /// have made any changes that need flushing.
    PreClose,
}

/// Trait used for all files.
pub trait File: Node {
    /// Capabilities:
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        false
    }
    fn executable(&self) -> bool {
        false
    }

    /// Called when the file is going to be accessed, typically by a new connection.
    /// Flags is the same as the flags passed to `fidl_fuchsia_io.Node/Open`.
    /// The following flags are handled by the connection and do not need to be handled inside
    /// open():
    /// * OPEN_FLAG_TRUNCATE - A call to truncate() will be made immediately after open().
    /// * OPEN_FLAG_DESCRIBE - The OnOpen event is sent before any other requests are received from
    /// the file's client.
    fn open_file(&self, options: &FileOptions) -> impl Future<Output = Result<(), Status>> + Send;

    /// Truncate the file to `length`.
    /// If there are pending attributes to update (see `update_attributes`), they should also be
    /// flushed at this time. Otherwise, no attributes should be updated, other than size as needed.
    fn truncate(&self, length: u64) -> impl Future<Output = Result<(), Status>> + Send;

    /// Get a VMO representing this file.
    /// If not supported by the underlying filesystem, should return Err(NOT_SUPPORTED).
    #[cfg(target_os = "fuchsia")]
    fn get_backing_memory(
        &self,
        flags: fio::VmoFlags,
    ) -> impl Future<Output = Result<zx::Vmo, Status>> + Send;

    /// Get the size of this file.
    /// This is used to calculate seek offset relative to the end.
    fn get_size(&self) -> impl Future<Output = Result<u64, Status>> + Send;

    /// Set the mutable attributes of this file based on the values in `attributes`. If the file
    /// does not support updating *all* of the specified attributes, implementations should fail
    /// with `ZX_ERR_NOT_SUPPORTED`.
    fn update_attributes(
        &self,
        attributes: fio::MutableNodeAttributes,
    ) -> impl Future<Output = Result<(), Status>> + Send;

    /// List this files extended attributes.
    fn list_extended_attributes(
        &self,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, Status>> + Send {
        ready(Err(Status::NOT_SUPPORTED))
    }

    /// Get the value for an extended attribute.
    fn get_extended_attribute(
        &self,
        _name: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Status>> + Send {
        ready(Err(Status::NOT_SUPPORTED))
    }

    /// Set the value for an extended attribute.
    fn set_extended_attribute(
        &self,
        _name: Vec<u8>,
        _value: Vec<u8>,
        _mode: fio::SetExtendedAttributeMode,
    ) -> impl Future<Output = Result<(), Status>> + Send {
        ready(Err(Status::NOT_SUPPORTED))
    }

    /// Remove the value for an extended attribute.
    fn remove_extended_attribute(
        &self,
        _name: Vec<u8>,
    ) -> impl Future<Output = Result<(), Status>> + Send {
        ready(Err(Status::NOT_SUPPORTED))
    }

    /// Preallocate disk space for this range.
    #[cfg(fuchsia_api_level_at_least = "HEAD")]
    fn allocate(
        &self,
        _offset: u64,
        _length: u64,
        _mode: fio::AllocateMode,
    ) -> impl Future<Output = Result<(), Status>> + Send {
        ready(Err(Status::NOT_SUPPORTED))
    }

    /// Set the merkle tree and the descriptor for this file and mark the file as fsverity-enabled.
    #[cfg(fuchsia_api_level_at_least = "HEAD")]
    fn enable_verity(
        &self,
        _options: fio::VerificationOptions,
    ) -> impl Future<Output = Result<(), Status>> + Send {
        ready(Err(Status::NOT_SUPPORTED))
    }

    /// Sync this file's contents to the storage medium (probably disk).
    /// This does not necessarily guarantee that the file will be completely written to disk once
    /// the call returns. It merely guarantees that any changes to the file have been propagated
    /// to the next layer in the storage stack.
    fn sync(&self, mode: SyncMode) -> impl Future<Output = Result<(), Status>> + Send;

    /// Returns an optional event for the file which signals `fuchsia.io2.FileSignal` events to
    /// clients (e.g. when a file becomes readable).  See `fuchsia.io2.File.Describe`.
    fn event(&self) -> Result<Option<fidl::Event>, Status> {
        Ok(None)
    }
}

// Trait for handling reads and writes to a file. Files that support Streams should handle reads and
// writes via a Pager instead of implementing this trait.
pub trait FileIo: Send + Sync {
    /// Read at most |buffer.len()| bytes starting at |offset| into |buffer|. The function may read
    /// less than |count| bytes and still return success, in which case read_at returns the number
    /// of bytes read into |buffer|.
    fn read_at(
        &self,
        offset: u64,
        buffer: &mut [u8],
    ) -> impl Future<Output = Result<u64, Status>> + Send;

    /// Write |content| starting at |offset|, returning the number of bytes that were successfully
    /// written.
    ///

    /// If there are pending attributes to update (see `update_attributes`), they should also be
    /// flushed at this time. Otherwise, no attributes should be updated, other than size as needed.
    fn write_at(
        &self,
        offset: u64,
        content: &[u8],
    ) -> impl Future<Output = Result<u64, Status>> + Send;

    /// Appends |content| returning, if successful, the number of bytes written, and the file offset
    /// after writing.  Implementations should make the writes atomic, so in the event that multiple
    /// requests to append are in-flight, it should appear that the two writes are applied in
    /// sequence.
    ///
    /// If there are pending attributes to update (see `update_attributes`), they should also be
    /// flushed at this time. Otherwise, no attributes should be updated, other than size as needed.
    fn append(&self, content: &[u8]) -> impl Future<Output = Result<(u64, u64), Status>> + Send;
}

/// Trait for dispatching read, write, and seek FIDL requests for a given connection. The
/// implementer of this trait is responsible for maintaining the per connection state.
///
/// Files that support Streams should handle reads and writes via a Pager instead of implementing
/// this trait.
pub trait RawFileIoConnection: Send + Sync {
    /// Reads at most `count` bytes from the file starting at the connection's seek offset and
    /// advances the seek offset.
    fn read(&self, count: u64) -> impl Future<Output = Result<Vec<u8>, Status>> + Send;

    /// Reads `count` bytes from the file starting at `offset`.
    fn read_at(
        &self,
        offset: u64,
        count: u64,
    ) -> impl Future<Output = Result<Vec<u8>, Status>> + Send;

    /// Writes `data` to the file starting at the connect's seek offset and advances the seek
    /// offset. If the connection is in append mode then the seek offset is moved to the end of the
    /// file before writing. Returns the number of bytes written.
    fn write(&self, data: &[u8]) -> impl Future<Output = Result<u64, Status>> + Send;

    /// Writes `data` to the file starting at `offset`. Returns the number of bytes written.
    fn write_at(
        &self,
        offset: u64,
        data: &[u8],
    ) -> impl Future<Output = Result<u64, Status>> + Send;

    /// Modifies the connection's seek offset. Returns the connections new seek offset.
    fn seek(
        &self,
        offset: i64,
        origin: fio::SeekOrigin,
    ) -> impl Future<Output = Result<u64, Status>> + Send;

    /// Notifies the `IoOpHandler` that the flags of the connection have changed.
    /// TODO(https://fxbug.dev/376509077): Migrate this to fio::Flags.
    fn update_flags(&self, flags: fio::OpenFlags) -> Status;
}

pub trait FileLike: Node {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        options: FileOptions,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status>;
}

/// Helper to open a file or node as required.
pub fn serve(
    file: Arc<impl FileLike>,
    scope: ExecutionScope,
    protocols: &impl ProtocolsExt,
    object_request: ObjectRequestRef<'_>,
) -> Result<(), Status> {
    if protocols.is_node() {
        let options = protocols.to_node_options(file.entry_info().type_())?;
        file.open_as_node(scope, options, object_request)
    } else {
        file.open(scope, protocols.to_file_options()?, object_request)
    }
}
