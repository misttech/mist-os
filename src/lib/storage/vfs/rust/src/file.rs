// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Module holding different kinds of files and their building blocks.
use {
    async_trait::async_trait,
    fidl_fuchsia_io as fio,
    fuchsia_zircon::{self as zx, Status},
};

/// File nodes backed by VMOs.
pub mod vmo;

pub mod test_utils;

mod common;

pub mod connection;

pub use connection::io1::{FidlIoConnection, GetVmo, RawIoConnection, StreamIoConnection};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FileOptions {
    pub rights: fio::Operations,
    pub is_node: bool,
    pub is_append: bool,
}

impl FileOptions {
    /// Converts to `StreamOptions`. Returns `None` if a stream should not be used for a connection
    /// with these options.
    pub fn to_stream_options(&self) -> Option<zx::StreamOptions> {
        if self.is_node {
            // Don't use streams when opening a connection as a node reference. Reading, writing, or
            // seeking on a node reference should return ZX_ERR_BAD_HANDLE. Giving a node reference
            // a stream with no rights will fail those requests with ZX_ERR_ACCESS_DENIED.
            return None;
        }
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
        Some(options)
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
        if self.is_node {
            flags |= fio::OpenFlags::NODE_REFERENCE;
        }
        if self.is_append {
            flags |= fio::OpenFlags::APPEND;
        }
        flags
    }
}

/// Trait used for all files.
#[async_trait]
pub trait File: Send + Sync {
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
    async fn open(&self, options: &FileOptions) -> Result<(), Status>;

    /// Truncate the file to |length|.
    /// If there are pending attributes to update (see set_attrs), they should also be flushed at
    /// this time.  Otherwise, no attributes should be updated, other than size as needed.
    async fn truncate(&self, length: u64) -> Result<(), Status>;

    /// Get a VMO representing this file.
    /// If not supported by the underlying filesystem, should return Error(NOT_SUPPORTED).
    async fn get_backing_memory(&self, flags: fio::VmoFlags) -> Result<zx::Vmo, Status>;

    /// Get the size of this file.
    /// This is used to calculate seek offset relative to the end.
    async fn get_size(&self) -> Result<u64, Status>;

    /// Get this file's attributes.
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, Status>;

    /// Returns node attributes (io2).
    async fn get_attributes(
        &self,
        _requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    /// Set the attributes of this file based on the values in `attrs`.
    async fn set_attrs(
        &self,
        flags: fio::NodeAttributeFlags,
        attrs: fio::NodeAttributes,
    ) -> Result<(), Status>;

    /// List this files extended attributes.
    async fn list_extended_attributes(&self) -> Result<Vec<Vec<u8>>, Status> {
        Err(Status::NOT_SUPPORTED)
    }

    /// Get the value for an extended attribute.
    async fn get_extended_attribute(&self, _name: Vec<u8>) -> Result<Vec<u8>, Status> {
        Err(Status::NOT_SUPPORTED)
    }

    /// Set the value for an extended attribute.
    async fn set_extended_attribute(&self, _name: Vec<u8>, _value: Vec<u8>) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    /// Remove the value for an extended attribute.
    async fn remove_extended_attribute(&self, _name: Vec<u8>) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    /// Called when the file is closed.
    /// This function will also do the equivalent of sync() before the returning.
    async fn close(&self) -> Result<(), Status>;

    /// Sync this file's contents to the storage medium (probably disk).
    /// This does not necessarily guarantee that the file will be completely written to disk once
    /// the call returns. It merely guarantees that any changes to the file have been propagated
    /// to the next layer in the storage stack.
    async fn sync(&self) -> Result<(), Status>;

    /// Returns information about the filesystem.
    fn query_filesystem(&self) -> Result<fio::FilesystemInfo, Status> {
        Err(Status::NOT_SUPPORTED)
    }

    /// Returns an optional event for the file which signals `fuchsia.io2.FileSignal` events to
    /// clients (e.g. when a file becomes readable).  See `fuchsia.io2.File.Describe`.
    fn event(&self) -> Result<Option<zx::Event>, Status> {
        Ok(None)
    }
}

// Trait for handling reads and writes to a file. Files that support Streams should handle reads and
// writes via a Pager instead of implementing this trait.
#[async_trait]
pub trait FileIo: Send + Sync {
    /// Read at most |buffer.len()| bytes starting at |offset| into |buffer|. The function may read
    /// less than |count| bytes and still return success, in which case read_at returns the number
    /// of bytes read into |buffer|.
    async fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<u64, Status>;

    /// Write |content| starting at |offset|, returning the number of bytes that were successfully
    /// written.
    /// If there are pending attributes to update (see set_attrs), they should also be flushed at
    /// this time.  Otherwise, no attributes should be updated, other than size as needed.
    async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, Status>;

    /// Appends |content| returning, if successful, the number of bytes written, and the file offset
    /// after writing.  Implementations should make the writes atomic, so in the event that multiple
    /// requests to append are in-flight, it should appear that the two writes are applied in
    /// sequence.
    /// If there are pending attributes to update (see set_attrs), they should also be flushed at
    /// this time.  Otherwise, no attributes should be updated, other than size as needed.
    async fn append(&self, content: &[u8]) -> Result<(u64, u64), Status>;
}

/// Trait for dispatching read, write, and seek FIDL requests for a given connection. The
/// implementater of this trait is responsible for maintaning the per connection state.
///
/// Files that support Streams should handle reads and writes via a Pager instead of implementing
/// this trait.
#[async_trait]
pub trait RawFileIoConnection: Send + Sync {
    /// Reads at most `count` bytes from the file starting at the connection's seek offset and
    /// advances the seek offset.
    async fn read(&self, count: u64) -> Result<Vec<u8>, zx::Status>;

    /// Reads `count` bytes from the file starting at `offset`.
    async fn read_at(&self, offset: u64, count: u64) -> Result<Vec<u8>, zx::Status>;

    /// Writes `data` to the file starting at the connect's seek offset and advances the seek
    /// offset. If the connection is in append mode then the seek offset is moved to the end of the
    /// file before writing. Returns the number of bytes written.
    async fn write(&self, data: &[u8]) -> Result<u64, zx::Status>;

    /// Writes `data` to the file starting at `offset`. Returns the number of bytes written.
    async fn write_at(&self, offset: u64, data: &[u8]) -> Result<u64, zx::Status>;

    /// Modifies the connection's seek offset. Returns the connections new seek offset.
    async fn seek(&self, offset: i64, origin: fio::SeekOrigin) -> Result<u64, zx::Status>;

    /// Notifies the `IoOpHandler` that the flags of the connection have changed.
    fn update_flags(&self, flags: fio::OpenFlags) -> zx::Status;
}
