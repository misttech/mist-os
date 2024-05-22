// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    directory::entry::{DirectoryEntry, EntryInfo, OpenRequest},
    execution_scope::ExecutionScope,
    file::{FidlIoConnection, File, FileIo, FileLike, FileOptions, SyncMode},
    node::Node,
    ObjectRequestRef,
};

use {
    async_trait::async_trait,
    fidl_fuchsia_io as fio,
    fuchsia_zircon_status::Status,
    std::sync::{Arc, Mutex},
};

#[cfg(test)]
mod tests;

// Redefine these constants as a u32 as in macos they are u16
const S_IRUSR: u32 = libc::S_IRUSR as u32;
const S_IWUSR: u32 = libc::S_IWUSR as u32;

/// A file with a byte array for content, useful for testing.
pub struct SimpleFile {
    data: Mutex<Vec<u8>>,
    writable: bool,
}

impl SimpleFile {
    /// Create a new read-only test file with the provided content.
    pub fn read_only(content: impl AsRef<[u8]>) -> Arc<Self> {
        Arc::new(SimpleFile { data: Mutex::new(content.as_ref().to_vec()), writable: false })
    }

    /// Create a new writable test file with the provided content.
    pub fn read_write(content: impl AsRef<[u8]>) -> Arc<Self> {
        Arc::new(SimpleFile { data: Mutex::new(content.as_ref().to_vec()), writable: true })
    }
}

impl DirectoryEntry for SimpleFile {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File)
    }

    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), Status> {
        request.open_file(self)
    }
}

#[async_trait]
impl Node for SimpleFile {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, Status> {
        let content_size = self.data.lock().unwrap().len().try_into().unwrap();
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_FILE | S_IRUSR | if self.writable { S_IWUSR } else { 0 },
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
        Ok(immutable_attributes!(
            requested_attributes,
            Immutable {
                protocols: fio::NodeProtocolKinds::FILE,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::READ_BYTES
                    | if self.writable {
                        fio::Operations::WRITE_BYTES
                    } else {
                        fio::Operations::empty()
                    },
                content_size: content_size,
                storage_size: content_size,
                link_count: 1,
                id: fio::INO_UNKNOWN,
            }
        ))
    }
}

impl FileIo for SimpleFile {
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

    async fn append(&self, content: &[u8]) -> Result<(u64, u64), Status> {
        if !self.writable {
            return Err(Status::ACCESS_DENIED);
        }

        let mut data = self.data.lock().unwrap();
        data.extend_from_slice(content);
        Ok((content.len().try_into().unwrap(), data.len().try_into().unwrap()))
    }
}

impl File for SimpleFile {
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

    async fn truncate(&self, length: u64) -> Result<(), Status> {
        if self.writable {
            self.data.lock().unwrap().resize(length as usize, 0);
            Ok(())
        } else {
            Err(Status::ACCESS_DENIED)
        }
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

impl FileLike for SimpleFile {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        options: FileOptions,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        FidlIoConnection::spawn(scope, self, options, object_request)
    }
}
