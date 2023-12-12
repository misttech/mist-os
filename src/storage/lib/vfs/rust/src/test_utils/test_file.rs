// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use fuchsia_zircon_status::Status;
use std::sync::{Arc, Mutex};

use crate::attributes;
use crate::directory::entry::{DirectoryEntry, EntryInfo};
use crate::execution_scope::ExecutionScope;
use crate::file::{FidlIoConnection, File, FileIo, FileOptions, SyncMode};
use crate::node::Node;
use crate::path::Path;
use crate::ToObjectRequest;

// Redefine these constants as a u32 as in macos they are u16
const S_IRUSR: u32 = libc::S_IRUSR as u32;
// const S_IXUSR: u32 = libc::S_IXUSR as u32;

/// A file with a byte array for content, useful for testing.
pub struct TestFile {
    data: Mutex<Vec<u8>>,
    writable: bool,
}

impl TestFile {
    /// Create a new read-only test file with the provided content.
    pub fn read_only(content: impl AsRef<[u8]>) -> Arc<Self> {
        Arc::new(TestFile { data: Mutex::new(content.as_ref().to_vec()), writable: false })
    }

    /// Create a new writable test file with the provided content.
    pub fn read_write(content: impl AsRef<[u8]>) -> Arc<Self> {
        Arc::new(TestFile { data: Mutex::new(content.as_ref().to_vec()), writable: true })
    }
}

impl DirectoryEntry for TestFile {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            if !path.is_empty() {
                return Err(Status::NOT_DIR);
            }

            if flags.intersects(fio::OpenFlags::APPEND) {
                return Err(Status::NOT_SUPPORTED);
            }

            object_request.take().spawn(&scope.clone(), move |object_request| {
                Box::pin(async move {
                    object_request.create_connection(scope, self, flags, FidlIoConnection::create)
                })
            });
            Ok(())
        });
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File)
    }
}

#[async_trait]
impl Node for TestFile {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, Status> {
        let content_size = self.data.lock().unwrap().len().try_into().unwrap();
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_FILE | S_IRUSR,
            id: fio::INO_UNKNOWN,
            content_size,
            storage_size: content_size,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, Status> {
        let content_size: u64 = self.data.lock().unwrap().len().try_into().unwrap();
        Ok(attributes!(
            requested_attributes,
            Mutable { creation_time: 0, modification_time: 0, mode: 0, uid: 0, gid: 0, rdev: 0 },
            Immutable {
                protocols: fio::NodeProtocolKinds::FILE,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::READ_BYTES
                    | fio::Operations::WRITE_BYTES,
                content_size: content_size,
                storage_size: content_size,
                link_count: 1,
                id: fio::INO_UNKNOWN,
            }
        ))
    }
}

#[async_trait]
impl FileIo for TestFile {
    async fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<u64, Status> {
        let content_size = self.data.lock().unwrap().len().try_into().unwrap();
        if offset >= content_size {
            return Ok(0u64);
        }
        let read_len: u64 = std::cmp::min(content_size - offset, buffer.len().try_into().unwrap());
        let read_len_usize: usize = read_len.try_into().unwrap();
        buffer[..read_len_usize].copy_from_slice(
            &self.data.lock().unwrap()[offset.try_into().unwrap()..][..read_len_usize],
        );
        Ok(read_len)
    }

    async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, Status> {
        if !self.writable {
            return Err(Status::ACCESS_DENIED);
        }

        let mut data = self.data.lock().unwrap();
        let offset = offset.try_into().unwrap();
        let data_len = data.len();
        data.resize(std::cmp::max(data_len, offset + content.len()), 0);
        data[offset..][..content.len()].copy_from_slice(content);
        Ok(content.len().try_into().unwrap())
    }

    async fn append(&self, _content: &[u8]) -> Result<(u64, u64), Status> {
        Err(Status::NOT_SUPPORTED)
    }
}

#[async_trait]
impl File for TestFile {
    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        self.writable
    }

    fn executable(&self) -> bool {
        false
    }

    async fn open_file(&self, _options: &FileOptions) -> Result<(), Status> {
        Ok(())
    }

    async fn truncate(&self, _length: u64) -> Result<(), Status> {
        Err(Status::ACCESS_DENIED)
    }

    async fn get_size(&self) -> Result<u64, Status> {
        Ok(self.data.lock().unwrap().len().try_into().unwrap())
    }

    #[cfg(target_os = "fuchsia")]
    async fn get_backing_memory(&self, _flags: fio::VmoFlags) -> Result<fidl::Vmo, Status> {
        Err(Status::NOT_SUPPORTED)
    }

    async fn set_attrs(
        &self,
        _flags: fio::NodeAttributeFlags,
        _attrs: fio::NodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    async fn update_attributes(
        &self,
        _attributes: fio::MutableNodeAttributes,
    ) -> Result<(), Status> {
        Err(Status::NOT_SUPPORTED)
    }

    async fn sync(&self, _mode: SyncMode) -> Result<(), Status> {
        Ok(())
    }
}
