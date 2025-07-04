// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::directory::FatDirectory;
use crate::filesystem::{FatFilesystem, FatFilesystemInner};
use crate::node::Node;
use crate::refs::{FatfsFileRef, Guard, GuardMut, Wrapper};
use crate::types::File;
use crate::util::{
    dos_date_to_unix_time, dos_to_unix_time, fatfs_error_to_status, unix_to_dos_time,
};
use fidl_fuchsia_io as fio;
use fuchsia_sync::RwLock;
use std::cell::RefCell;
use std::fmt::Debug;
use std::io::{Read, Seek, Write};
use std::pin::Pin;
use std::sync::Arc;
use vfs::attributes;
use vfs::directory::entry::EntryInfo;
use vfs::file::{File as VfsFile, FileIo as VfsFileIo, FileOptions, SyncMode};
use zx::{self as zx, Status};

fn extend(file: &mut File<'_>, mut current: u64, target: u64) -> Result<(), Status> {
    let zeros = vec![0; 8192];
    while current < target {
        let to_do = (std::cmp::min(target, (current + 8192) / 8192 * 8192) - current) as usize;
        let written = file.write(&zeros[..to_do]).map_err(fatfs_error_to_status)? as u64;
        if written == 0 {
            return Err(Status::NO_SPACE);
        }
        current += written;
    }
    Ok(())
}

fn seek_for_write(file: &mut File<'_>, offset: u64) -> Result<(), Status> {
    if offset > fatfs::MAX_FILE_SIZE as u64 {
        return Err(Status::INVALID_ARGS);
    }
    let real_offset = file.seek(std::io::SeekFrom::Start(offset)).map_err(fatfs_error_to_status)?;
    if real_offset == offset {
        return Ok(());
    }
    assert!(real_offset < offset);
    let result = extend(file, real_offset, offset);
    if let Err(e) = result {
        // Return the file to its original size.
        file.seek(std::io::SeekFrom::Start(real_offset)).map_err(fatfs_error_to_status)?;
        file.truncate().map_err(fatfs_error_to_status)?;
        return Err(e);
    }
    Ok(())
}

struct FatFileData {
    name: String,
    parent: Option<Arc<FatDirectory>>,
}

/// Represents a single file on the disk.
pub struct FatFile {
    file: RefCell<FatfsFileRef>,
    filesystem: Pin<Arc<FatFilesystem>>,
    data: RwLock<FatFileData>,
}

// The only member that isn't `Sync + Send` is the `file` member.
// `file` is protected by the lock on `filesystem`, so we can safely
// implement Sync + Send for FatFile.
unsafe impl Sync for FatFile {}
unsafe impl Send for FatFile {}

impl FatFile {
    /// Create a new FatFile.
    pub(crate) fn new(
        file: FatfsFileRef,
        parent: Arc<FatDirectory>,
        filesystem: Pin<Arc<FatFilesystem>>,
        name: String,
    ) -> Arc<Self> {
        Arc::new(FatFile {
            file: RefCell::new(file),
            filesystem,
            data: RwLock::new(FatFileData { parent: Some(parent), name }),
        })
    }

    /// Borrow the underlying Fatfs File mutably.
    pub(crate) fn borrow_file_mut<'a>(
        &'a self,
        fs: &'a FatFilesystemInner,
    ) -> Option<GuardMut<'a, FatfsFileRef>> {
        let mut file = self.file.borrow_mut();
        if file.get_mut(fs).is_none() {
            None
        } else {
            Some(GuardMut::new(fs, file))
        }
    }

    pub fn borrow_file<'a>(
        &'a self,
        fs: &'a FatFilesystemInner,
    ) -> Result<Guard<'a, FatfsFileRef>, Status> {
        let file = self.file.borrow();
        if file.get(fs).is_none() {
            Err(Status::BAD_HANDLE)
        } else {
            Ok(Guard::new(fs, file))
        }
    }

    async fn write_or_append(
        &self,
        offset: Option<u64>,
        content: &[u8],
    ) -> Result<(u64, u64), Status> {
        let fs_lock = self.filesystem.lock();
        let mut file = self.borrow_file_mut(&fs_lock).ok_or(Status::BAD_HANDLE)?;
        let mut file_offset = match offset {
            Some(offset) => {
                seek_for_write(&mut *file, offset)?;
                offset
            }
            None => file.seek(std::io::SeekFrom::End(0)).map_err(fatfs_error_to_status)?,
        };
        let mut total_written = 0;
        while total_written < content.len() {
            let written = file.write(&content[total_written..]).map_err(fatfs_error_to_status)?;
            if written == 0 {
                break;
            }
            total_written += written;
            file_offset += written as u64;
            let result = file.write(&content[total_written..]).map_err(fatfs_error_to_status);
            match result {
                Ok(0) => break,
                Ok(written) => {
                    total_written += written;
                    file_offset += written as u64;
                }
                Err(e) => {
                    if total_written > 0 {
                        break;
                    }
                    return Err(e);
                }
            }
        }
        self.filesystem.mark_dirty();
        Ok((total_written as u64, file_offset))
    }
}

impl Node for FatFile {
    /// Flush to disk and invalidate the reference that's contained within this FatFile.
    /// Any operations on the file will return Status::BAD_HANDLE until it is re-attached.
    fn detach(&self, fs: &FatFilesystemInner) {
        self.file.borrow_mut().take(fs);
    }

    /// Attach to the given parent and re-open the underlying `FatfsFileRef` this file represents.
    fn attach(
        &self,
        new_parent: Arc<FatDirectory>,
        name: &str,
        fs: &FatFilesystemInner,
    ) -> Result<(), Status> {
        let mut data = self.data.write();
        data.name = name.to_owned();
        // Safe because we hold the fs lock.
        let mut file = self.file.borrow_mut();
        // Safe because we have a reference to the FatFilesystem.
        unsafe { file.maybe_reopen(fs, &new_parent, name)? };
        data.parent.replace(new_parent);
        Ok(())
    }

    fn did_delete(&self) {
        self.data.write().parent.take();
    }

    fn open_ref(&self, fs_lock: &FatFilesystemInner) -> Result<(), Status> {
        let data = self.data.read();
        let mut file_ref = self.file.borrow_mut();
        unsafe { file_ref.open(&fs_lock, data.parent.as_deref(), &data.name) }
    }

    /// Close the underlying FatfsFileRef, regardless of the number of open connections.
    fn shut_down(&self, fs: &FatFilesystemInner) -> Result<(), Status> {
        self.file.borrow_mut().take(fs);
        Ok(())
    }

    fn flush_dir_entry(&self, fs: &FatFilesystemInner) -> Result<(), Status> {
        if let Some(mut file) = self.borrow_file_mut(fs) {
            file.flush_dir_entry().map_err(fatfs_error_to_status)?
        }
        Ok(())
    }

    fn close_ref(&self, fs: &FatFilesystemInner) {
        self.file.borrow_mut().close(fs);
    }
}

impl vfs::node::Node for FatFile {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, Status> {
        let fs_lock = self.filesystem.lock();
        let file = self.borrow_file(&fs_lock)?;
        let content_size = file.len() as u64;
        let creation_time = dos_to_unix_time(file.created());
        let modification_time = dos_to_unix_time(file.modified());
        let access_time = dos_date_to_unix_time(file.accessed());

        // Figure out the storage size by rounding content_size up to the nearest
        // multiple of cluster_size.
        let cluster_size = fs_lock.cluster_size() as u64;
        let storage_size = ((content_size + cluster_size - 1) / cluster_size) * cluster_size;

        Ok(attributes!(
            requested_attributes,
            Mutable {
                creation_time: creation_time,
                modification_time: modification_time,
                access_time: access_time
            },
            Immutable {
                protocols: fio::NodeProtocolKinds::FILE,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::READ_BYTES
                    | fio::Operations::WRITE_BYTES,
                content_size: content_size,
                storage_size: storage_size,
                link_count: 1, // FAT does not support hard links, so there is always 1 "link".
            }
        ))
    }

    fn close(self: Arc<Self>) {
        self.close_ref(&self.filesystem.lock());
    }

    fn query_filesystem(&self) -> Result<fio::FilesystemInfo, Status> {
        self.filesystem.query_filesystem()
    }

    fn will_clone(&self) {
        self.open_ref(&self.filesystem.lock()).unwrap();
    }
}

impl Debug for FatFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FatFile").field("name", &self.data.read().name).finish()
    }
}

impl VfsFile for FatFile {
    fn writable(&self) -> bool {
        return true;
    }

    async fn open_file(&self, _options: &FileOptions) -> Result<(), Status> {
        Ok(())
    }

    async fn truncate(&self, length: u64) -> Result<(), Status> {
        let fs_lock = self.filesystem.lock();
        let mut file = self.borrow_file_mut(&fs_lock).ok_or(Status::BAD_HANDLE)?;
        seek_for_write(&mut *file, length)?;
        file.truncate().map_err(fatfs_error_to_status)?;
        self.filesystem.mark_dirty();
        Ok(())
    }

    // Unfortunately, fatfs has deprecated the "set_created" and "set_modified" methods,
    // saying that a TimeProvider should be used instead. There doesn't seem to be a good way to
    // use a TimeProvider to change the creation/modification time of a file after the fact,
    // so we need to use the deprecated methods.
    #[allow(deprecated)]
    async fn update_attributes(
        &self,
        attributes: fio::MutableNodeAttributes,
    ) -> Result<(), Status> {
        const SUPPORTED_MUTABLE_ATTRIBUTES: fio::NodeAttributesQuery =
            fio::NodeAttributesQuery::CREATION_TIME
                .union(fio::NodeAttributesQuery::MODIFICATION_TIME);

        if !SUPPORTED_MUTABLE_ATTRIBUTES
            .contains(vfs::common::mutable_node_attributes_to_query(&attributes))
        {
            return Err(Status::NOT_SUPPORTED);
        }

        let fs_lock = self.filesystem.lock();
        let mut file = self.borrow_file_mut(&fs_lock).ok_or(Status::BAD_HANDLE)?;
        let mut needs_flush = false;
        if let Some(creation_time) = attributes.creation_time {
            file.set_created(unix_to_dos_time(creation_time));
            needs_flush = true;
        }
        if let Some(modification_time) = attributes.modification_time {
            file.set_modified(unix_to_dos_time(modification_time));
            needs_flush = true;
        }

        if needs_flush {
            file.flush().map_err(fatfs_error_to_status)?;
            self.filesystem.mark_dirty();
        }
        Ok(())
    }

    async fn get_size(&self) -> Result<u64, Status> {
        let fs_lock = self.filesystem.lock();
        let file = self.borrow_file(&fs_lock)?;
        Ok(file.len() as u64)
    }

    async fn sync(&self, _mode: SyncMode) -> Result<(), Status> {
        let fs_lock = self.filesystem.lock();
        let mut file = self.borrow_file_mut(&fs_lock).ok_or(Status::BAD_HANDLE)?;

        file.flush().map_err(fatfs_error_to_status)?;
        Ok(())
    }
}

impl VfsFileIo for FatFile {
    async fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<u64, Status> {
        let fs_lock = self.filesystem.lock();
        let mut file = self.borrow_file_mut(&fs_lock).ok_or(Status::BAD_HANDLE)?;

        let real_offset =
            file.seek(std::io::SeekFrom::Start(offset)).map_err(fatfs_error_to_status)?;
        // Technically, we don't need to do this because the read should return zero bytes later,
        // but it's better to be explicit.
        if real_offset != offset {
            return Ok(0);
        }
        let mut total_read = 0;
        while total_read < buffer.len() {
            let read = file.read(&mut buffer[total_read..]).map_err(fatfs_error_to_status)?;
            if read == 0 {
                break;
            }
            total_read += read;
        }
        Ok(total_read as u64)
    }

    async fn write_at(&self, offset: u64, content: &[u8]) -> Result<u64, Status> {
        self.write_or_append(Some(offset), content).await.map(|r| r.0)
    }

    async fn append(&self, content: &[u8]) -> Result<(u64, u64), Status> {
        self.write_or_append(None, content).await
    }
}

impl vfs::directory::entry::GetEntryInfo for FatFile {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File)
    }
}

#[cfg(test)]
mod tests {
    // We only test things here that aren't covered by fs_tests.
    use super::*;
    use crate::node::{Closer, FatNode};
    use crate::tests::{TestDiskContents, TestFatDisk};

    const TEST_DISK_SIZE: u64 = 2048 << 10; // 2048K
    const TEST_FILE_CONTENT: &str = "test file contents";

    struct TestFile(Arc<FatFile>);

    impl TestFile {
        fn new() -> Self {
            let disk = TestFatDisk::empty_disk(TEST_DISK_SIZE);
            let structure =
                TestDiskContents::dir().add_child("test_file", TEST_FILE_CONTENT.into());
            structure.create(&disk.root_dir());

            let fs = disk.into_fatfs();
            let dir = fs.get_fatfs_root();
            let mut closer = Closer::new(&fs.filesystem());
            dir.open_ref(&fs.filesystem().lock()).expect("open_ref failed");
            closer.add(FatNode::Dir(dir.clone()));
            let file = match dir
                .open_child("test_file", fio::OpenFlags::empty(), &mut closer)
                .expect("Open to succeed")
            {
                FatNode::File(f) => f,
                val => panic!("Unexpected value {:?}", val),
            };
            file.open_ref(&fs.filesystem().lock()).expect("open_ref failed");
            TestFile(file)
        }
    }

    impl Drop for TestFile {
        fn drop(&mut self) {
            self.0.close_ref(&self.0.filesystem.lock());
        }
    }

    impl std::ops::Deref for TestFile {
        type Target = Arc<FatFile>;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    #[fuchsia::test]
    async fn test_read_at() {
        let file = TestFile::new();
        // Note: fatfs incorrectly casts u64 to i64, which causes this value to wrap
        // around and become negative, which causes seek() in read_at() to fail.
        // The error is not particularly important, because fat has a maximum 32-bit file size.
        // An error like this will only happen if an application deliberately seeks to a (very)
        // out-of-range position or reads at a nonsensical offset.
        let mut buffer = [0u8; 512];
        let err = file.read_at(u64::MAX - 30, &mut buffer).await.expect_err("Read fails");
        assert_eq!(err, Status::INVALID_ARGS);
    }

    #[fuchsia::test]
    async fn test_get_attributes() {
        let file = TestFile::new();
        let fio::NodeAttributes2 { mutable_attributes, immutable_attributes } =
            vfs::node::Node::get_attributes(&**file, fio::NodeAttributesQuery::all())
                .await
                .unwrap();
        assert_eq!(immutable_attributes.content_size.unwrap(), TEST_FILE_CONTENT.len() as u64);
        assert!(immutable_attributes.storage_size.unwrap() > TEST_FILE_CONTENT.len() as u64);
        assert_eq!(immutable_attributes.protocols.unwrap(), fio::NodeProtocolKinds::FILE);
        assert_eq!(
            immutable_attributes.abilities.unwrap(),
            fio::Abilities::GET_ATTRIBUTES
                | fio::Abilities::UPDATE_ATTRIBUTES
                | fio::Abilities::READ_BYTES
                | fio::Abilities::WRITE_BYTES
        );
        assert!(mutable_attributes.creation_time.is_some());
        assert!(mutable_attributes.modification_time.is_some());
    }

    #[fuchsia::test]
    async fn test_update_attributes() {
        let file = TestFile::new();

        let new_time = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("SystemTime before UNIX EPOCH")
            .as_nanos();
        let new_attrs = fio::MutableNodeAttributes {
            creation_time: Some(new_time.try_into().unwrap()),
            ..Default::default()
        };
        file.update_attributes(new_attrs).await.expect("update attributes failed");
    }
}
