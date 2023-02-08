// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        fuse_attr::to_fxfs_time,
        fuse_errors::FuseErrorParser,
        fuse_fs::{FuseFs, FuseStrParser},
    },
    async_trait::async_trait,
    fuchsia as _,
    fuse3::{
        raw::prelude::{Filesystem as FuseFilesystem, *},
        Result,
    },
    futures as _,
    futures_util::stream::Iter,
    fxfs::{
        filesystem::Filesystem,
        log::info,
        object_handle::{ObjectHandle, ReadObjectHandle, WriteObjectHandle},
        object_store::{
            directory::{replace_child, ReplacedChild},
            transaction::{Options, TransactionHandler},
            ObjectDescriptor, Timestamp,
        },
    },
    std::{ffi::OsStr, io::Write, time::Duration, vec::IntoIter},
};

/// The TTL duration of each reply.
const TTL: Duration = Duration::from_secs(1);

/// Some of the function parameters (i.e., request, mode, mask, fh, flags and lock_owner)
/// are not supported by Fxfs and hence are ignored.
#[async_trait]
impl FuseFilesystem for FuseFs {
    // Stream of directory entries without attributes.
    type DirEntryStream = Iter<IntoIter<Result<DirectoryEntry>>>;

    // Stream of directory entries with attributes.
    type DirEntryPlusStream = Iter<IntoIter<Result<DirectoryEntryPlus>>>;

    /// Initialize filesystem. Called before any other filesystem method.
    async fn init(&self, _req: Request) -> Result<()> {
        info!("init");
        Ok(())
    }

    /// Gracefully close filesystem. Currently fuse3 cannot handle unmount gracefully,
    /// see https://github.com/Sherlock-Holo/fuse3/issues/52.
    /// To fix it, destroy() is triggered when SIGTERM is received by the FUSE-Fxfs program.
    async fn destroy(&self, _req: Request) {
        info!("destroy");
        self.fs.close().await.expect("close failed");
    }

    /// Look up a entry called `name` under `parent` directory and get its attributes.
    /// Return Ok with object's attributes if successful.
    /// Return ENOENT if `parent` does not exist or `name` is not found under `parent`.
    async fn lookup(&self, _req: Request, parent: u64, name: &OsStr) -> Result<ReplyEntry> {
        let parent = self.fuse_inode_to_object_id(parent);
        info!("lookup (parent={:?}, name={:?}, name_len={:?})", parent, name, name.len());
        let dir = self.open_dir(parent).await?;
        let object = dir.lookup(name.parse_str()?).await.parse_error()?;

        if let Some((object_id, object_descriptor)) = object {
            Ok(ReplyEntry {
                ttl: TTL,
                attr: self.create_object_attr(object_id, object_descriptor).await?,
                generation: 0,
            })
        } else {
            Err(libc::ENOENT.into())
        }
    }

    /// Create a directory called `name` under `parent` directory.
    /// Return Ok with the new directory's attributes if successful.
    /// Return ENOENT if `parent` does not exist.
    /// Return ENOTDIR if `parent` is not a directory.
    /// Return EEXIST if `name` already exists under `parent` directory.
    async fn mkdir(
        &self,
        _req: Request,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
    ) -> Result<ReplyEntry> {
        let parent = self.fuse_inode_to_object_id(parent);
        info!("mkdir (parent={:?}, name={:?})", parent, name);
        let dir = self.open_dir(parent).await?;

        if dir.lookup(name.parse_str()?).await.parse_error()?.is_none() {
            let mut transaction =
                self.fs.clone().new_transaction(&[], Options::default()).await.parse_error()?;

            let child_dir =
                dir.create_child_dir(&mut transaction, name.parse_str()?).await.parse_error()?;
            transaction.commit().await.parse_error()?;

            Ok(ReplyEntry {
                ttl: TTL,
                attr: self
                    .create_object_attr(child_dir.object_id(), ObjectDescriptor::Directory)
                    .await?,
                generation: 0,
            })
        } else {
            Err(libc::EEXIST.into())
        }
    }

    /// Remove a file or symlink called `name` under `parent` directory.
    /// Return Ok if the object is successfully removed.
    /// Return ENOTDIR if `parent` is not a directory.
    /// Return EISDIR if `name` is a directory under `parent`.
    /// Return ENOENT if `name` is not found under `parent`.
    async fn unlink(&self, _req: Request, parent: u64, name: &OsStr) -> Result<()> {
        let parent = self.fuse_inode_to_object_id(parent);
        info!("unlink (parent={:?}, name={:?})", parent, name);
        let dir = self.open_dir(parent).await?;

        if let Some((_, object_descriptor)) = dir.lookup(name.parse_str()?).await.parse_error()? {
            if object_descriptor == ObjectDescriptor::File {
                let mut transaction =
                    self.fs.clone().new_transaction(&[], Options::default()).await.parse_error()?;
                let replaced_child =
                    replace_child(&mut transaction, None, (&dir, name.parse_str()?))
                        .await
                        .parse_error()?;
                transaction.commit().await.parse_error()?;

                // If the object is a file without remaining links,
                // immediately tombstones it in the graveyard.
                if let ReplacedChild::File(object_id) = replaced_child {
                    self.fs
                        .graveyard()
                        .tombstone(dir.store().store_object_id(), object_id)
                        .await
                        .parse_error()?;
                }

                Ok(())
            } else if object_descriptor == ObjectDescriptor::Symlink {
                let mut transaction =
                    self.fs.clone().new_transaction(&[], Options::default()).await.parse_error()?;
                replace_child(&mut transaction, None, (&dir, name.parse_str()?))
                    .await
                    .parse_error()?;

                transaction.commit().await.parse_error()?;
                Ok(())
            } else {
                Err(libc::EISDIR.into())
            }
        } else {
            Err(libc::ENOENT.into())
        }
    }

    /// Read data from a file object with id `inode`,
    /// which starts from `offset` with a length of `size`.
    /// Return Ok with the read data if successful.
    /// Return ENOENT if `inode` does not exist.
    /// Return EISDIR if `inode` is a directory.
    async fn read(
        &self,
        _req: Request,
        inode: u64,
        _fh: u64,
        offset: u64,
        size: u32,
    ) -> Result<ReplyData> {
        let inode = self.fuse_inode_to_object_id(inode);
        info!("read (inode={:?}, offset={:?}, size={:?})", inode, offset, size);

        if let Some(object_type) = self.get_object_type(inode).await? {
            if object_type == ObjectDescriptor::File {
                let handle = self.get_object_handle(inode).await?;
                let mut out: Vec<u8> = Vec::new();
                let align = offset % self.fs.block_size();

                let mut buf = handle.allocate_buffer(handle.block_size() as usize);
                // Round down for the block alignment.
                let mut ofs = offset - align;
                let len = size as u64 + align + ofs;

                loop {
                    let bytes = handle.read(ofs, buf.as_mut()).await.parse_error()?;
                    if len - ofs > bytes as u64 {
                        // Read `bytes` size of content from buf.
                        ofs += bytes as u64;
                        out.write_all(&buf.as_ref().as_slice()[..bytes])?;
                        if bytes as u64 != handle.block_size() {
                            break;
                        }
                    } else {
                        // Read the remaining content from buf.
                        out.write_all(&buf.as_ref().as_slice()[..(len - ofs) as usize])?;
                        break;
                    }
                }
                let out: Vec<u8> = if (align as usize) < out.len() {
                    out.drain((align as usize)..).collect()
                } else {
                    vec![]
                };

                Ok(ReplyData { data: out.into() })
            } else {
                Err(libc::EISDIR.into())
            }
        } else {
            Err(libc::ENOENT.into())
        }
    }

    /// Write data into a file object with id `inode`, starting from `offset`.
    /// Return Ok with the length of written data if successful.
    /// Return EISDIR if `inode` is a directory.
    /// Return ENOENT if `inode` does not exist.
    async fn write(
        &self,
        _req: Request,
        inode: u64,
        _fh: u64,
        offset: u64,
        data: &[u8],
        _flags: u32,
    ) -> Result<ReplyWrite> {
        let inode = self.fuse_inode_to_object_id(inode);
        info!("write (inode={:?}, offset={:?}, data_len={:?})", inode, offset, data.len());

        if let Some(object_type) = self.get_object_type(inode).await? {
            if object_type == ObjectDescriptor::File {
                let handle = self.get_object_handle(inode).await?;
                let mut buf = handle.allocate_buffer(data.len());

                buf.as_mut_slice().copy_from_slice(data);
                handle.write_or_append(Some(offset), buf.as_ref()).await.parse_error()?;
                handle.flush().await.parse_error()?;

                Ok(ReplyWrite { written: data.len() as u32 })
            } else {
                Err(libc::EISDIR.into())
            }
        } else {
            Err(libc::ENOENT.into())
        }
    }

    /// Release an open file object with id `inode` to indicate its end of read or write.
    /// Parameter `flush` is unused because data is flushed at every read/write.
    /// Return OK and remove `inode`'s file type and handle from cache if successful.
    /// Return EISDIR if `inode` is a directory.
    /// Return ENOENT if `inode` does not exist.
    async fn release(
        &self,
        _req: Request,
        inode: u64,
        _fh: u64,
        _flags: u32,
        _lock_owner: u64,
        _flush: bool,
    ) -> Result<()> {
        let inode = self.fuse_inode_to_object_id(inode);
        info!("release (inode={:?})", inode);

        if let Some(object_type) = self.get_object_type(inode).await? {
            if object_type == ObjectDescriptor::File {
                // TODO(fxbug.dev/117461): Add cache support that removes object handle from cache.
                Ok(())
            } else {
                Err(libc::EISDIR.into())
            }
        } else {
            Err(libc::ENOENT.into())
        }
    }

    /// Forget an object with id `inode`.
    /// This is only called for objects with a limited lifetime, and hence not needed in Fxfs.
    /// TODO(fxbug.dev/117461): Implement this after Fxfs objects support limied lifetime.
    async fn forget(&self, _req: Request, inode: u64, _nlookup: u64) {
        let inode = self.fuse_inode_to_object_id(inode);
        info!("forget (inode={:?})", inode);
    }

    /// Get attributes of an object with id `inode`.
    /// Return Ok with `inode`'s attributes if successful.
    /// Return ENOENT if `inode` does not exist.
    async fn getattr(
        &self,
        _req: Request,
        inode: u64,
        _fh: Option<u64>,
        _flags: u32,
    ) -> Result<ReplyAttr> {
        let inode = self.fuse_inode_to_object_id(inode);
        info!("getattr (inode={:?})", inode);

        if let Some(object_type) = self.get_object_type(inode).await? {
            Ok(ReplyAttr { ttl: TTL, attr: self.create_object_attr(inode, object_type).await? })
        } else {
            Err(libc::ENOENT.into())
        }
    }

    /// Set attributes of an object with id `inode` including its timestamps and object size.
    /// Return Ok with `inode`'s new attributes if successful.
    /// Return ENOENT if `inode` does not exist.
    async fn setattr(
        &self,
        _req: Request,
        inode: u64,
        _fh: Option<u64>,
        set_attr: SetAttr,
    ) -> Result<ReplyAttr> {
        let inode = self.fuse_inode_to_object_id(inode);
        info!(
            "setattr (inode={:?}, size={:?}, ctime={:?}, mtime={:?})",
            inode, set_attr.size, set_attr.ctime, set_attr.mtime
        );

        if let Some(object_type) = self.get_object_type(inode).await? {
            let ctime: Option<Timestamp> = match set_attr.ctime {
                Some(t) => Some(to_fxfs_time(t)),
                None => None,
            };
            let mtime: Option<Timestamp> = match set_attr.mtime {
                Some(t) => Some(to_fxfs_time(t)),
                None => None,
            };

            if object_type == ObjectDescriptor::File {
                let handle = self.get_object_handle(inode).await?;
                let mut transaction =
                    self.fs.clone().new_transaction(&[], Options::default()).await.parse_error()?;
                handle.write_timestamps(&mut transaction, ctime, mtime).await.parse_error()?;
                transaction.commit().await.parse_error()?;

                // Truncate the file size if size attribute needs to be set.
                if let Some(size) = set_attr.size {
                    handle.truncate(size).await.parse_error()?;
                    handle.flush().await.parse_error()?;
                }
            } else if object_type == ObjectDescriptor::Directory {
                let dir = self.open_dir(inode).await?;
                let mut transaction =
                    self.fs.clone().new_transaction(&[], Options::default()).await.parse_error()?;
                dir.update_attributes(&mut transaction, ctime, mtime, |_| {})
                    .await
                    .parse_error()?;
                transaction.commit().await.parse_error()?;
            }

            Ok(ReplyAttr { ttl: TTL, attr: self.create_object_attr(inode, object_type).await? })
        } else {
            Err(libc::ENOENT.into())
        }
    }

    /// Set an extended attribute of an object with id `inode`.
    /// Fxfs currently doesn't support this feature.
    /// TODO(fxbug.dev/117461): Add support for setting extended attributes.
    async fn setxattr(
        &self,
        _req: Request,
        inode: u64,
        name: &OsStr,
        _value: &[u8],
        _flags: u32,
        _position: u32,
    ) -> Result<()> {
        info!("setxattr {:?}, {:?}", inode, name);
        Err(libc::ENOSYS.into())
    }

    /// Get an extended attribute of an object with id `inode`.
    /// Fxfs currently doesn't support this feature.
    /// TODO(fxbug.dev/117461): Add support for getting extended attributes.
    async fn getxattr(
        &self,
        _req: Request,
        inode: u64,
        name: &OsStr,
        size: u32,
    ) -> Result<ReplyXAttr> {
        info!("getxattr {:?}, {:?} {:?}", inode, name, size);
        Err(libc::ENOSYS.into())
    }

    /// Check access permission of `inode`.
    /// Return Ok if `inode` exists.
    /// Return ENOENT if `inode` does not exist.
    async fn access(&self, _req: Request, inode: u64, _mask: u32) -> Result<()> {
        let inode = self.fuse_inode_to_object_id(inode);
        info!("access (inode={:?})", inode);

        if let Some(_) = self.get_object_type(inode).await? {
            Ok(())
        } else {
            Err(libc::ENOENT.into())
        }
    }

    /// Create a file called `name` under `parent` directory.
    /// Return Ok with the new file's attributes if successful.
    /// Return ENOENT if `parent` does not exist.
    /// Return ENOTDIR if `parent` is not a directory.
    /// Return EEXIST if `name` already exists under `parent`.
    async fn create(
        &self,
        _req: Request,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        flags: u32,
    ) -> Result<ReplyCreated> {
        let parent = self.fuse_inode_to_object_id(parent);
        info!("create (parent={:?}, name={:?})", parent, name);

        let dir = self.open_dir(parent).await?;
        if dir.lookup(name.parse_str()?).await.parse_error()?.is_none() {
            let mut transaction =
                self.fs.clone().new_transaction(&[], Options::default()).await.parse_error()?;

            let child_file =
                dir.create_child_file(&mut transaction, name.parse_str()?).await.parse_error()?;

            transaction.commit().await.parse_error()?;

            // TODO(fxbug.dev/117461): Add cache supoort to store object handle.

            Ok(ReplyCreated {
                ttl: TTL,
                attr: self
                    .create_object_attr(child_file.object_id(), ObjectDescriptor::File)
                    .await?,
                generation: 0,
                fh: 0,
                flags,
            })
        } else {
            Err(libc::EEXIST.into())
        }
    }

    /// Open a file object with id `inode` for read or write.
    /// Return Ok and store file's handle and type in cache if successful.
    /// Return ENOENT if `inode` does not exist.
    /// Return EISDIR if `inode` is a directory.
    async fn open(&self, _req: Request, inode: u64, _flags: u32) -> Result<ReplyOpen> {
        let inode = self.fuse_inode_to_object_id(inode);
        info!("open (inode={:?})", inode);
        let object_type = self.get_object_type(inode).await?;

        if let Some(descriptor) = object_type {
            if descriptor == ObjectDescriptor::File {
                // TODO(fxbug.dev/117461): Add cache supoort to store object handle.

                Ok(ReplyOpen { fh: 0, flags: 0 })
            } else {
                Err(libc::EISDIR.into())
            }
        } else {
            Err(libc::ENOENT.into())
        }
    }

    /// Allocate space for a file with id `inode`, starting from position
    /// `offset` with size `length`.
    /// Return Ok if the space is successfully allocated.
    /// Return EISDIR if `inode` is a directory.
    /// Return ENOENT if `inode` does not exist.
    async fn fallocate(
        &self,
        _req: Request,
        inode: u64,
        _fh: u64,
        offset: u64,
        length: u64,
        _mode: u32,
    ) -> Result<()> {
        let inode = self.fuse_inode_to_object_id(inode);
        info!("fallocate (inode={:?}, offset={:?}, length={:?})", inode, offset, length);

        if let Some(object_type) = self.get_object_type(inode).await? {
            if object_type == ObjectDescriptor::File {
                let handle = self.get_object_handle(inode).await?;
                handle.truncate(offset + length).await.parse_error()?;
                handle.flush().await.parse_error()?;
                Ok(())
            } else {
                Err(libc::EISDIR.into())
            }
        } else {
            Err(libc::ENOENT.into())
        }
    }

    /// Find next data or hole after the specified offset with seek option `whence`
    /// in a file with id `inode`.
    /// Return `offset` if `whence` is SEEK_CUR or SEEK_SET.
    /// Return the length of remaining content if `whence` is SEEK_END.
    /// Return EISDIR if `inode` is not a file.
    /// Return ENOENT if `inode` does not exist.
    async fn lseek(
        &self,
        _req: Request,
        inode: u64,
        _fh: u64,
        offset: u64,
        whence: u32,
    ) -> Result<ReplyLSeek> {
        let inode = self.fuse_inode_to_object_id(inode);
        info!("lseek (inode={:?})", inode);
        let whence = whence as i32;

        if let Some(object_type) = self.get_object_type(inode).await? {
            if object_type == ObjectDescriptor::File {
                let offset = if whence == libc::SEEK_CUR || whence == libc::SEEK_SET {
                    offset
                } else if whence == libc::SEEK_END {
                    let content_size = self
                        .get_object_properties(inode, ObjectDescriptor::File)
                        .await?
                        .data_attribute_size;
                    if content_size >= offset {
                        content_size - offset
                    } else {
                        0
                    }
                } else {
                    return Err(libc::EINVAL.into());
                };
                Ok(ReplyLSeek { offset })
            } else {
                Err(libc::EISDIR.into())
            }
        } else {
            Err(libc::ENOENT.into())
        }
    }

    /// Copy a range of data from `inode` starting from position `off_in` into
    /// `inode_out` starting from position `off_out` with a size of `length`.
    /// Return Ok with copied length if successful.
    /// Return EISDIR if `inode` or `inode_out` is not a file.
    /// Return ENOENT if `inode` or `inode_out` does not exist.
    async fn copy_file_range(
        &self,
        req: Request,
        inode: u64,
        fh_in: u64,
        off_in: u64,
        inode_out: u64,
        fh_out: u64,
        off_out: u64,
        length: u64,
        flags: u64,
    ) -> Result<ReplyCopyFileRange> {
        let inode = self.fuse_inode_to_object_id(inode);
        let inode_out = self.fuse_inode_to_object_id(inode_out);
        info!("copy_file_range (inode={:?}, inode_out={:?})", inode, inode_out);

        let data = self.read(req, inode, fh_in, off_in, length as _).await?;
        let data = data.data.as_ref();
        let ReplyWrite { written } =
            self.write(req, inode_out, fh_out, off_out, data, flags as _).await?;

        Ok(ReplyCopyFileRange { copied: u64::from(written) })
    }

    /// Read symlink with id `inode`.
    /// Return Ok with link of the symlink in bytes.
    /// Return ENOENT if `inode` does not exist or `inode` is not a symlink.
    async fn readlink(&self, _req: Request, inode: u64) -> Result<ReplyData> {
        let inode = self.fuse_inode_to_object_id(inode);
        info!("readlink (inode={:?})", inode);
        if let Some(link) = self.root_dir().await?.read_symlink(inode).await.parse_error()? {
            let out: Vec<u8> = link.as_bytes().to_vec();
            Ok(ReplyData { data: out.into() })
        } else {
            Err(libc::ENOENT.into())
        }
    }

    /// Create a symlink called `name` under `parent` linking to `link`.
    /// Return the attributes of symlink if successful.
    /// Return ENOTDIR if `parent` is not a directory.
    /// Return EEXIST if `name` already exists under `parent`.
    /// Return ENOENT if `parent` does not exist.
    async fn symlink(
        &self,
        _req: Request,
        parent: u64,
        name: &OsStr,
        link: &OsStr,
    ) -> Result<ReplyEntry> {
        let parent = self.fuse_inode_to_object_id(parent);
        info!("symlink (parent={:?}, name={:?}, link={:?})", parent, name, link);

        let dir = self.open_dir(parent).await?;
        let mut transaction =
            self.fs.clone().new_transaction(&[], Options::default()).await.parse_error()?;

        let symlink_id = dir
            .create_symlink(&mut transaction, link.parse_str()?, name.parse_str()?)
            .await
            .parse_error()?;

        transaction.commit().await.parse_error()?;
        Ok(ReplyEntry {
            ttl: TTL,
            attr: self.create_object_attr(symlink_id, ObjectDescriptor::Symlink).await?,
            generation: 0,
        })
    }

    /// Create a hard link called `new_name` under `new_parent` linking to `inode`.
    /// Return the updated attributes of `inode` if successful.
    /// Return ENOTDIR if `parent` is not a directory.
    /// Return EEXIST if `name` already exists under `parent`.
    /// Return ENOENT if `parent` or `inode` does not exist.
    /// Return EISDIR if `inode` is a directory.
    async fn link(
        &self,
        _req: Request,
        inode: u64,
        new_parent: u64,
        new_name: &OsStr,
    ) -> Result<ReplyEntry> {
        let inode = self.fuse_inode_to_object_id(inode);
        let new_parent = self.fuse_inode_to_object_id(new_parent);
        info!("link (inode={:?}, new_parent={:?}, new_name={:?})", inode, new_parent, new_name);

        if let Some(object_type) = self.get_object_type(inode).await? {
            if object_type == ObjectDescriptor::File {
                let dir = self.open_dir(new_parent).await?;

                if dir.lookup(new_name.parse_str()?).await.parse_error()?.is_none() {
                    let mut transaction = self
                        .fs
                        .clone()
                        .new_transaction(&[], Options::default())
                        .await
                        .parse_error()?;

                    dir.insert_child(
                        &mut transaction,
                        new_name.parse_str()?,
                        inode,
                        ObjectDescriptor::File,
                    )
                    .await
                    .parse_error()?;

                    // Increase the number of refs to the file by 1.
                    self.default_store
                        .adjust_refs(&mut transaction, inode, 1)
                        .await
                        .parse_error()?;
                    transaction.commit().await.parse_error()?;

                    Ok(ReplyEntry {
                        ttl: TTL,
                        attr: self.create_object_attr(inode, ObjectDescriptor::File).await?,
                        generation: 0,
                    })
                } else {
                    Err(libc::EEXIST.into())
                }
            } else {
                Err(libc::EISDIR.into())
            }
        } else {
            Err(libc::ENOENT.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::fuse_fs::{FuseFs, FuseStrParser},
        fuse3::{
            raw::{Filesystem, Request},
            Errno, FileType, SetAttr, Timestamp,
        },
        fxfs::{filesystem::Filesystem as FxFs, object_store::ObjectDescriptor},
        std::ffi::OsStr,
    };

    const DEFAULT_FILE_MODE: u32 = 0o755;
    const DEFAULT_FLAG: u32 = 0;
    const DEFAULT_TIME: Timestamp = Timestamp { sec: 10i64, nsec: 20u32 };
    const INVALID_INODE: u64 = 0;
    const TEST_DATA: &[u8] = b"hello";

    /// Create fake request for testing purpose.
    fn new_fake_request() -> Request {
        Request { unique: 0, uid: 0, gid: 0, pid: 0 }
    }

    #[fuchsia::test]
    async fn test_mkdir_create_directory_tree() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let mkdir_reply = fs
            .mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let (foo_id, foo_descriptor) =
            dir.lookup(OsStr::new("foo").parse_str().unwrap()).await.unwrap().unwrap();
        assert_eq!(foo_descriptor, ObjectDescriptor::Directory);
        assert_eq!(mkdir_reply.attr.ino, foo_id);
        assert_eq!(mkdir_reply.attr.kind, FileType::Directory);

        let _mkdir_reply = fs
            .mkdir(new_fake_request(), foo_id, OsStr::new("bar"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let child_dir = fs.open_dir(foo_id).await.expect("open_dir failed");
        let (bar_id, bar_descriptor) =
            child_dir.lookup(OsStr::new("bar").parse_str().unwrap()).await.unwrap().unwrap();
        assert_eq!(bar_descriptor, ObjectDescriptor::Directory);
        assert_eq!(_mkdir_reply.attr.ino, bar_id);
        assert_eq!(_mkdir_reply.attr.kind, FileType::Directory);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_mkdir_fails_when_parent_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let mkdir_res = fs
            .mkdir(new_fake_request(), INVALID_INODE, OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await;
        assert_eq!(mkdir_res, Err(libc::ENOENT.into()));

        let lookup_res = dir.lookup(OsStr::new("foo").parse_str().unwrap()).await.unwrap();
        assert_eq!(lookup_res, None);
        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_mkdir_fails_when_directory_already_exists() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        fs.mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let mkdir_res = fs
            .mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await;
        assert_eq!(mkdir_res, Err(libc::EEXIST.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_mkdir_fails_when_file_already_exists() {
        let fs = FuseFs::new_in_memory().await;

        let dir = fs.root_dir().await.expect("root_dir failed");
        fs.create(
            new_fake_request(),
            dir.object_id(),
            OsStr::new("foo"),
            DEFAULT_FILE_MODE,
            DEFAULT_FLAG,
        )
        .await
        .expect("create file failed");
        let mkdir_res = fs
            .mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await;
        assert_eq!(mkdir_res, Err(libc::EEXIST.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_lookup_search_for_directory() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let mkdir_reply = fs
            .mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");

        let lookup_reply = fs
            .lookup(new_fake_request(), dir.object_id(), OsStr::new("foo"))
            .await
            .expect("lookup failed");

        assert_eq!(lookup_reply.attr.ino, mkdir_reply.attr.ino);
        assert_eq!(lookup_reply.attr.kind, FileType::Directory);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_lookup_search_for_file() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let file_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create file failed");
        let lookup_reply = fs
            .lookup(new_fake_request(), dir.object_id(), OsStr::new("foo"))
            .await
            .expect("lookup failed");

        assert_eq!(lookup_reply.attr.ino, file_reply.attr.ino);
        assert_eq!(lookup_reply.attr.kind, FileType::RegularFile);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_lookup_search_for_symlink() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let symlink_reply = fs
            .symlink(new_fake_request(), dir.object_id(), OsStr::new("link"), OsStr::new("foo"))
            .await
            .expect("symlink failed");
        let lookup_reply = fs
            .lookup(new_fake_request(), dir.object_id(), OsStr::new("link"))
            .await
            .expect("lookup failed");

        assert_eq!(lookup_reply.attr.ino, symlink_reply.attr.ino);
        assert_eq!(lookup_reply.attr.kind, FileType::Symlink);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_lookup_fails_when_parent_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;
        let lookup_res = fs.lookup(new_fake_request(), INVALID_INODE, OsStr::new("foo")).await;
        assert_eq!(lookup_res, Err(libc::ENOENT.into()));
        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_lookup_fails_when_name_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");
        let lookup_res = fs.lookup(new_fake_request(), dir.object_id(), OsStr::new("foo")).await;
        assert_eq!(lookup_res, Err(libc::ENOENT.into()));
        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_unlink_remove_file() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let file_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create file failed");
        let unlink_res = fs.unlink(new_fake_request(), dir.object_id(), OsStr::new("foo")).await;
        let lookup_res = dir.lookup(OsStr::new("foo").parse_str().unwrap()).await.unwrap();
        let type_res =
            fs.get_object_type(file_reply.attr.ino).await.expect("get_object_type failed");

        assert_eq!(unlink_res, Ok(()));
        assert_eq!(lookup_res, None);
        assert_eq!(type_res, None);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_unlink_remove_symlink() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");
        let symlin_reply = fs
            .symlink(new_fake_request(), dir.object_id(), OsStr::new("link"), OsStr::new("foo"))
            .await
            .expect("symlink failed");

        let unlink_res = fs.unlink(new_fake_request(), dir.object_id(), OsStr::new("link")).await;
        let lookup_res = dir.lookup(OsStr::new("link").parse_str().unwrap()).await.unwrap();
        let type_res =
            fs.get_object_type(symlin_reply.attr.ino).await.expect("get_object_type failed");

        assert_eq!(unlink_res, Ok(()));
        assert_eq!(lookup_res, None);
        assert_eq!(type_res, None);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_unlink_fails_when_object_is_directory() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");
        fs.mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");

        let unlink_res = fs.unlink(new_fake_request(), dir.object_id(), OsStr::new("foo")).await;
        assert_eq!(unlink_res, Err(libc::EISDIR.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_unlink_fails_when_parent_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;

        let unlink_res = fs.unlink(new_fake_request(), INVALID_INODE, OsStr::new("foo")).await;
        assert_eq!(unlink_res, Err(libc::ENOENT.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_create_new_file() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let (child_id, child_descriptor) =
            dir.lookup(OsStr::new("foo").parse_str().unwrap()).await.unwrap().unwrap();
        assert_eq!(child_descriptor, ObjectDescriptor::File);
        assert_eq!(create_reply.attr.ino, child_id);
        assert_eq!(create_reply.attr.kind, FileType::RegularFile);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_create_fails_when_parent_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_res = fs
            .create(new_fake_request(), INVALID_INODE, OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await;
        assert_eq!(create_res, Err(libc::ENOENT.into()));

        let lookup_res = dir.lookup(OsStr::new("foo").parse_str().unwrap()).await.unwrap();
        assert_eq!(lookup_res, None);
        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_create_fails_when_file_already_exists() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        fs.create(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let create_res = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await;
        assert_eq!(create_res, Err(libc::EEXIST.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_create_fails_when_directory_already_exists() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        fs.mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let create_res = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await;
        assert_eq!(create_res, Err(libc::EEXIST.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_open_file() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let open_res = fs.open(new_fake_request(), create_reply.attr.ino, DEFAULT_FLAG).await;
        assert_eq!(open_res.is_ok(), true);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_open_fails_when_file_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;

        let open_res = fs.open(new_fake_request(), INVALID_INODE, DEFAULT_FLAG).await;
        assert_eq!(open_res, Err(libc::ENOENT.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_open_fails_when_object_is_directory() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let mkdir_reply = fs
            .mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let open_res = fs.open(new_fake_request(), mkdir_reply.attr.ino, DEFAULT_FLAG).await;
        assert_eq!(open_res, Err(libc::EISDIR.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_read_with_same_length_as_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        assert_eq!(read_reply.data, TEST_DATA);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_read_with_offset_smaller_than_block_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 1, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        assert_eq!(read_reply.data, TEST_DATA[1..]);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_read_with_offset_equal_to_block_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        let read_reply = fs
            .read(
                new_fake_request(),
                create_reply.attr.ino,
                0,
                fs.fs.block_size(),
                TEST_DATA.len() as _,
            )
            .await
            .expect("read failed");
        assert_eq!(read_reply.data.len(), 0);

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        assert_eq!(read_reply.data, TEST_DATA);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_read_with_offset_larger_than_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 512, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        assert_eq!(read_reply.data.len(), 0);

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        assert_eq!(read_reply.data, TEST_DATA);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_read_with_smaller_length_than_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, (TEST_DATA.len() / 2) as _)
            .await
            .expect("read failed");
        assert_eq!(read_reply.data, TEST_DATA[0..TEST_DATA.len() / 2]);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_read_with_non_zero_offset_and_length_smaller_than_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 1, (TEST_DATA.len() / 2) as _)
            .await
            .expect("read failed");
        assert_eq!(read_reply.data, TEST_DATA[1..TEST_DATA.len() / 2 + 1]);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_read_with_length_larger_than_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, (TEST_DATA.len() * 2) as _)
            .await
            .expect("read failed");
        assert_eq!(read_reply.data, TEST_DATA);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_read_with_non_zero_offset_and_length_larger_than_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 1, (TEST_DATA.len() * 2) as _)
            .await
            .expect("read failed");
        assert_eq!(read_reply.data, TEST_DATA[1..]);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_read_when_file_is_empty() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        assert_eq!(read_reply.data.len(), 0);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_read_fails_when_file_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;

        let read_res = fs.read(new_fake_request(), INVALID_INODE, 0, 0, TEST_DATA.len() as _).await;
        assert_eq!(read_res.is_err(), true);
        let err: Errno = libc::ENOENT.into();
        assert_eq!(read_res.err().unwrap(), err);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_read_fails_when_object_is_directory() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let mkdir_reply = fs
            .mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let read_res =
            fs.read(new_fake_request(), mkdir_reply.attr.ino, 0, 0, TEST_DATA.len() as _).await;
        assert_eq!(read_res.is_err(), true);
        let err: Errno = libc::EISDIR.into();
        assert_eq!(read_res.err().unwrap(), err);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_write_to_file() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, TEST_DATA.len() as u32);

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        assert_eq!(read_reply.data, TEST_DATA);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_write_with_offset_smaller_than_file_size_and_file_size_unchanged() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, TEST_DATA.len() as u32);

        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 1, b"aa", DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, 2u32);

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        let result_data: &[u8] = b"haalo";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_write_with_offset_smaller_than_file_size_and_file_size_changed() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, TEST_DATA.len() as u32);

        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 4, b"aaa", DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, 3u32);

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, (TEST_DATA.len() + 2) as _)
            .await
            .expect("read failed");
        let result_data: &[u8] = b"hellaaa";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_write_with_offset_equal_to_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, TEST_DATA.len() as u32);

        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 5, b"aaa", DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, 3u32);

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, (TEST_DATA.len() + 3) as _)
            .await
            .expect("read failed");
        let result_data: &[u8] = b"helloaaa";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_write_with_offset_larger_than_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, TEST_DATA.len() as u32);

        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 6, b"aaa", DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, 3u32);

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, (TEST_DATA.len() + 4) as _)
            .await
            .expect("read failed");
        let result_data: &[u8] = b"hello\0aaa";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_write_when_file_is_empty() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 0, b"", DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, 0u32);

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        assert_eq!(read_reply.data.len(), 0);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_write_fails_when_file_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;

        let write_res =
            fs.write(new_fake_request(), INVALID_INODE, 0, 0, TEST_DATA, DEFAULT_FLAG).await;
        assert_eq!(write_res.is_err(), true);
        let err: Errno = libc::ENOENT.into();
        assert_eq!(write_res.err().unwrap(), err);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_write_fails_when_object_is_directory() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let mkdir_reply = fs
            .mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let write_res =
            fs.write(new_fake_request(), mkdir_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG).await;
        assert_eq!(write_res.is_err(), true);
        let err: Errno = libc::EISDIR.into();
        assert_eq!(write_res.err().unwrap(), err);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_getattr_on_directory() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let mkdir_reply = fs
            .mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let attr_reply = fs
            .getattr(new_fake_request(), mkdir_reply.attr.ino, Some(0), DEFAULT_FLAG)
            .await
            .expect("getattr failed");
        assert_eq!(attr_reply.attr.ino, mkdir_reply.attr.ino);
        assert_eq!(attr_reply.attr.kind, FileType::Directory);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_getattr_on_file() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let attr_reply = fs
            .getattr(new_fake_request(), create_reply.attr.ino, Some(0), DEFAULT_FLAG)
            .await
            .expect("getattr failed");
        assert_eq!(attr_reply.attr.ino, create_reply.attr.ino);
        assert_eq!(attr_reply.attr.kind, FileType::RegularFile);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_getattr_on_symlink() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let symlink_reply = fs
            .symlink(new_fake_request(), dir.object_id(), OsStr::new("link"), OsStr::new("foo"))
            .await
            .expect("create failed");
        let attr_reply = fs
            .getattr(new_fake_request(), symlink_reply.attr.ino, Some(0), DEFAULT_FLAG)
            .await
            .expect("getattr failed");
        assert_eq!(attr_reply.attr.ino, symlink_reply.attr.ino);
        assert_eq!(attr_reply.attr.kind, FileType::Symlink);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_getattr_fails_when_object_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;

        let attr_res = fs.getattr(new_fake_request(), INVALID_INODE, Some(0), DEFAULT_FLAG).await;
        assert_eq!(attr_res, Err(libc::ENOENT.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_setattr_on_directory_with_time() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let mkdir_reply = fs
            .mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let mut set_attr = SetAttr::default();
        set_attr.mtime = Some(DEFAULT_TIME);

        let setattr_reply = fs
            .setattr(new_fake_request(), mkdir_reply.attr.ino, Some(0), set_attr)
            .await
            .expect("setattr failed");
        assert_eq!(setattr_reply.attr.mtime, DEFAULT_TIME);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_setattr_on_file_with_time() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let mut set_attr = SetAttr::default();
        set_attr.mtime = Some(DEFAULT_TIME);

        let setattr_reply = fs
            .setattr(new_fake_request(), create_reply.attr.ino, Some(0), set_attr)
            .await
            .expect("setattr failed");
        assert_eq!(setattr_reply.attr.mtime, DEFAULT_TIME);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_setattr_on_file_with_zero_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, TEST_DATA.len() as u32);
        let mut set_attr = SetAttr::default();
        set_attr.size = Some(0u64);

        let setattr_reply = fs
            .setattr(new_fake_request(), create_reply.attr.ino, Some(0), set_attr)
            .await
            .expect("setattr failed");
        assert_eq!(setattr_reply.attr.size, 0);

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        let result_data: &[u8] = b"";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_setattr_on_file_with_shrinked_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, TEST_DATA.len() as u32);
        let mut set_attr = SetAttr::default();
        set_attr.size = Some(2u64);

        let setattr_reply = fs
            .setattr(new_fake_request(), create_reply.attr.ino, Some(0), set_attr)
            .await
            .expect("setattr failed");
        assert_eq!(setattr_reply.attr.size, 2);

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        let result_data: &[u8] = b"he";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_setattr_on_file_with_increased_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, TEST_DATA.len() as u32);
        let mut set_attr = SetAttr::default();
        set_attr.size = Some(8u64);

        let setattr_reply = fs
            .setattr(new_fake_request(), create_reply.attr.ino, Some(0), set_attr)
            .await
            .expect("setattr failed");
        assert_eq!(setattr_reply.attr.size, 8);

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        let result_data: &[u8] = b"hello";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_setattr_on_symlink_with_time() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let symlink_reply = fs
            .symlink(new_fake_request(), dir.object_id(), OsStr::new("link"), OsStr::new("foo"))
            .await
            .expect("symlink failed");
        let mut set_attr = SetAttr::default();
        set_attr.mtime = Some(DEFAULT_TIME);

        let setattr_reply = fs
            .setattr(new_fake_request(), symlink_reply.attr.ino, Some(0), set_attr)
            .await
            .expect("setattr failed");
        assert_eq!(setattr_reply.attr.ino, symlink_reply.attr.ino);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_setattr_fails_when_object_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;

        let mut set_attr = SetAttr::default();
        set_attr.atime = Some(DEFAULT_TIME);
        let attr_res = fs.setattr(new_fake_request(), INVALID_INODE, Some(0), set_attr).await;
        assert_eq!(attr_res, Err(libc::ENOENT.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_fallocate_alllocate_space_to_file() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("mkdir failed");
        fs.fallocate(new_fake_request(), create_reply.attr.ino, 0, 0, 128, DEFAULT_FILE_MODE)
            .await
            .expect("fallocate failed");

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_fallocate_allocate_zero_space_to_file() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        fs.fallocate(new_fake_request(), create_reply.attr.ino, 0, 0, 0, DEFAULT_FILE_MODE)
            .await
            .expect("fallocate failed");

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_fallocate_fails_when_file_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;

        let fallocate_res =
            fs.fallocate(new_fake_request(), INVALID_INODE, 0, 0, 128, DEFAULT_FILE_MODE).await;
        assert_eq!(fallocate_res, Err(libc::ENOENT.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_fallocate_fails_when_object_is_directory() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let mkdir_reply = fs
            .mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let fallocate_res = fs
            .fallocate(new_fake_request(), mkdir_reply.attr.ino, 0, 0, 128, DEFAULT_FILE_MODE)
            .await;
        assert_eq!(fallocate_res, Err(libc::EISDIR.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_fallocate_with_offset_smaller_than_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("mkdir failed");

        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, TEST_DATA.len() as u32);

        fs.fallocate(new_fake_request(), create_reply.attr.ino, 0, 3, 128, DEFAULT_FILE_MODE)
            .await
            .expect("fallocate failed");

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        let result_data: &[u8] = b"hello";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_fallocate_with_offset_larger_than_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("mkdir failed");

        let write_reply = fs
            .write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");
        assert_eq!(write_reply.written, TEST_DATA.len() as u32);

        fs.fallocate(
            new_fake_request(),
            create_reply.attr.ino,
            0,
            TEST_DATA.len() as u64 + 2,
            128,
            DEFAULT_FILE_MODE,
        )
        .await
        .expect("fallocate failed");

        let read_reply = fs
            .read(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA.len() as _)
            .await
            .expect("read failed");
        let result_data: &[u8] = b"hello";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_lseek_seek_for_cur_in_file() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        let lseek_reply = fs
            .lseek(new_fake_request(), create_reply.attr.ino, 0, 0, libc::SEEK_CUR as u32)
            .await
            .expect("lseek failed");
        assert_eq!(lseek_reply.offset, 0);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_lseek_seek_for_cur_in_file_with_offset() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        let lseek_reply = fs
            .lseek(new_fake_request(), create_reply.attr.ino, 0, 1, libc::SEEK_CUR as u32)
            .await
            .expect("lseek failed");
        assert_eq!(lseek_reply.offset, 1);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_lseek_seek_for_end_with_offset_smaller_than_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        let lseek_reply = fs
            .lseek(new_fake_request(), create_reply.attr.ino, 0, 1, libc::SEEK_END as u32)
            .await
            .expect("lseek failed");
        assert_eq!(lseek_reply.offset, 4);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_lseek_seek_for_end_with_offset_larger_than_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        let lseek_reply = fs
            .lseek(new_fake_request(), create_reply.attr.ino, 0, 8, libc::SEEK_END as u32)
            .await
            .expect("lseek failed");
        assert_eq!(lseek_reply.offset, 0);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_copy_file_range_from_one_file() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let create_reply_new = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("bar"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        fs.write(new_fake_request(), create_reply_new.attr.ino, 0, 0, b"aaaaaa", DEFAULT_FLAG)
            .await
            .expect("write failed");

        // Copy the data from the start of a file A to the start of another file B,
        // with the exact length of the data.
        // The data of A should be completely copied to B.
        let copy_reply = fs
            .copy_file_range(
                new_fake_request(),
                create_reply.attr.ino,
                0,
                0,
                create_reply_new.attr.ino,
                0,
                0,
                TEST_DATA.len() as _,
                0,
            )
            .await
            .expect("copy_file_range failed");
        assert_eq!(copy_reply.copied, TEST_DATA.len() as u64);

        let read_reply = fs
            .read(new_fake_request(), create_reply_new.attr.ino, 0, 0, (TEST_DATA.len() + 1) as _)
            .await
            .expect("read failed");

        let result_data: &[u8] = b"helloa";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_copy_file_range_with_input_offset_smaller_than_input_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let create_reply_new = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("bar"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        fs.write(new_fake_request(), create_reply_new.attr.ino, 0, 0, b"aaaaaa", DEFAULT_FLAG)
            .await
            .expect("write failed");

        // Copy the data from offset 1 of a file A to the start of another file B,
        // with the exact length of the data.
        // The data starting from offset 1 of A should be copied to B.
        let copy_reply = fs
            .copy_file_range(
                new_fake_request(),
                create_reply.attr.ino,
                0,
                1,
                create_reply_new.attr.ino,
                0,
                0,
                (TEST_DATA.len() - 1) as _,
                0,
            )
            .await
            .expect("copy_file_range failed");
        assert_eq!(copy_reply.copied, (TEST_DATA.len() - 1) as u64);

        let read_reply = fs
            .read(new_fake_request(), create_reply_new.attr.ino, 0, 0, (TEST_DATA.len() + 1) as _)
            .await
            .expect("read failed");

        let result_data: &[u8] = b"elloaa";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_copy_file_range_with_input_offset_larger_than_input_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let create_reply_new = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("bar"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        fs.write(new_fake_request(), create_reply_new.attr.ino, 0, 0, b"aaaaaa", DEFAULT_FLAG)
            .await
            .expect("write failed");

        // Copy the data from the end of a file A to the start of another file B,
        // with the exact length of the data.
        // No data should be copied to B.
        let copy_reply = fs
            .copy_file_range(
                new_fake_request(),
                create_reply.attr.ino,
                0,
                TEST_DATA.len() as _,
                create_reply_new.attr.ino,
                0,
                0,
                TEST_DATA.len() as _,
                0,
            )
            .await
            .expect("copy_file_range failed");
        assert_eq!(copy_reply.copied, 0u64);

        let read_reply = fs
            .read(new_fake_request(), create_reply_new.attr.ino, 0, 0, (TEST_DATA.len() + 1) as _)
            .await
            .expect("read failed");

        let result_data: &[u8] = b"aaaaaa";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_copy_file_range_with_output_offset_smaller_than_output_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let create_reply_new = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("bar"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        fs.write(new_fake_request(), create_reply_new.attr.ino, 0, 0, b"aaaaaa", DEFAULT_FLAG)
            .await
            .expect("write failed");

        // Copy the data from the start of a file A to offset 1 of another file B,
        // with the exact length of the data.
        // The data of A should be copied to B starting from B's offset 1.
        let copy_reply = fs
            .copy_file_range(
                new_fake_request(),
                create_reply.attr.ino,
                0,
                0,
                create_reply_new.attr.ino,
                0,
                1,
                TEST_DATA.len() as _,
                0,
            )
            .await
            .expect("copy_file_range failed");
        assert_eq!(copy_reply.copied, TEST_DATA.len() as u64);

        let read_reply = fs
            .read(new_fake_request(), create_reply_new.attr.ino, 0, 0, (TEST_DATA.len() + 1) as _)
            .await
            .expect("read failed");

        let result_data: &[u8] = b"ahello";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_copy_file_range_with_output_offset_larger_than_output_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let create_reply_new = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("bar"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        fs.write(new_fake_request(), create_reply_new.attr.ino, 0, 0, b"aaaaaa", DEFAULT_FLAG)
            .await
            .expect("write failed");

        // Copy the data from the start of a file A to the end of another file B,
        // with the exact length of the data.
        // The data of A should be copied and appended to the end of B.
        let copy_reply = fs
            .copy_file_range(
                new_fake_request(),
                create_reply.attr.ino,
                0,
                0,
                create_reply_new.attr.ino,
                0,
                7,
                TEST_DATA.len() as _,
                0,
            )
            .await
            .expect("copy_file_range failed");
        assert_eq!(copy_reply.copied, 5u64);

        let read_reply = fs
            .read(new_fake_request(), create_reply_new.attr.ino, 0, 0, 12)
            .await
            .expect("read failed");

        let result_data: &[u8] = b"aaaaaa\0hello";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_copy_file_range_with_copied_length_smaller_than_input_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let create_reply_new = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("bar"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        fs.write(new_fake_request(), create_reply_new.attr.ino, 0, 0, b"aaaaaa", DEFAULT_FLAG)
            .await
            .expect("write failed");

        // Copy the data from the start of a file A to the start of another file B,
        // with a length of one byte.
        // The first byte of data in A should be copied to B.
        let copy_reply = fs
            .copy_file_range(
                new_fake_request(),
                create_reply.attr.ino,
                0,
                0,
                create_reply_new.attr.ino,
                0,
                0,
                1,
                0,
            )
            .await
            .expect("copy_file_range failed");
        assert_eq!(copy_reply.copied, 1u64);

        let read_reply = fs
            .read(new_fake_request(), create_reply_new.attr.ino, 0, 0, (TEST_DATA.len() + 1) as _)
            .await
            .expect("read failed");

        let result_data: &[u8] = b"haaaaa";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_copy_file_range_with_copied_length_larger_than_input_file_size() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let create_reply_new = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("bar"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        fs.write(new_fake_request(), create_reply_new.attr.ino, 0, 0, b"aaaaaa", DEFAULT_FLAG)
            .await
            .expect("write failed");

        // Copy the data from the start of a file A to the start of another file B,
        // with a length larger than the size of A.
        // The data of A should be copied to B.
        let copy_reply = fs
            .copy_file_range(
                new_fake_request(),
                create_reply.attr.ino,
                0,
                0,
                create_reply_new.attr.ino,
                0,
                0,
                20,
                0,
            )
            .await
            .expect("copy_file_range failed");
        assert_eq!(copy_reply.copied, 5u64);

        let read_reply = fs
            .read(new_fake_request(), create_reply_new.attr.ino, 0, 0, (TEST_DATA.len() + 1) as _)
            .await
            .expect("read failed");

        let result_data: &[u8] = b"helloa";
        assert_eq!(read_reply.data, result_data);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_copy_file_range_fails_when_input_file_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply_new = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("bar"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply_new.attr.ino, 0, 0, b"aaaaaa", DEFAULT_FLAG)
            .await
            .expect("write failed");

        // Copy the data from a file that does not exist.
        let copy_res = fs
            .copy_file_range(
                new_fake_request(),
                INVALID_INODE,
                0,
                0,
                create_reply_new.attr.ino,
                0,
                0,
                TEST_DATA.len() as _,
                0,
            )
            .await;
        assert_eq!(copy_res, Err(libc::ENOENT.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_copy_file_range_when_output_file_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        fs.write(new_fake_request(), create_reply.attr.ino, 0, 0, TEST_DATA, DEFAULT_FLAG)
            .await
            .expect("write failed");

        // Copy the data from the start of a file to another file which does not exist.
        let copy_res = fs
            .copy_file_range(
                new_fake_request(),
                create_reply.attr.ino,
                0,
                0,
                INVALID_INODE,
                0,
                0,
                TEST_DATA.len() as _,
                0,
            )
            .await;
        assert_eq!(copy_res, Err(libc::ENOENT.into()));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_symlink_create_new_symlink() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let symlink_reply = fs
            .symlink(new_fake_request(), dir.object_id(), OsStr::new("link"), OsStr::new("foo"))
            .await
            .expect("symlink failed");
        let readlink_reply =
            fs.readlink(new_fake_request(), symlink_reply.attr.ino).await.expect("readlink failed");
        let link: Vec<u8> = "foo".to_owned().as_bytes().to_vec();
        assert_eq!(readlink_reply.data, link);

        let lookup_reply = fs
            .lookup(new_fake_request(), dir.object_id(), OsStr::new("link"))
            .await
            .expect("lookup failed");
        assert_eq!(lookup_reply.attr.ino, symlink_reply.attr.ino);

        let type_res =
            fs.get_object_type(symlink_reply.attr.ino).await.expect("get_object_type failed");
        assert_eq!(type_res, Some(ObjectDescriptor::Symlink));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_symlink_fails_when_parent_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;

        let symlink_res = fs
            .symlink(new_fake_request(), INVALID_INODE, OsStr::new("link"), OsStr::new("foo"))
            .await;

        assert_eq!(symlink_res.is_err(), true);
        let err: Errno = libc::ENOENT.into();
        assert_eq!(symlink_res.err().unwrap(), err);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_hardlink_create_hardlink_on_file() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");
        let hardlink_reply = fs
            .link(new_fake_request(), create_reply.attr.ino, dir.object_id(), OsStr::new("bar"))
            .await
            .expect("link failed");
        assert_eq!(hardlink_reply.attr.ino, create_reply.attr.ino);

        let lookup_reply = fs
            .lookup(new_fake_request(), dir.object_id(), OsStr::new("bar"))
            .await
            .expect("lookup failed");
        assert_eq!(lookup_reply.attr.ino, create_reply.attr.ino);

        let type_res =
            fs.get_object_type(hardlink_reply.attr.ino).await.expect("get_object_type failed");
        assert_eq!(type_res, Some(ObjectDescriptor::File));

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_hardlink_fails_when_parent_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        let hardlink_res = fs
            .link(new_fake_request(), create_reply.attr.ino, INVALID_INODE, OsStr::new("bar"))
            .await;

        assert_eq!(hardlink_res.is_err(), true);
        let err: Errno = libc::ENOENT.into();
        assert_eq!(hardlink_res.err().unwrap(), err);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_hardlink_fails_when_file_to_link_does_not_exist() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let hardlink_res =
            fs.link(new_fake_request(), INVALID_INODE, dir.object_id(), OsStr::new("bar")).await;

        assert_eq!(hardlink_res.is_err(), true);
        let err: Errno = libc::ENOENT.into();
        assert_eq!(hardlink_res.err().unwrap(), err);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_hardlink_fails_when_name_already_exists() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create failed");

        let hardlink_res = fs
            .link(new_fake_request(), create_reply.attr.ino, dir.object_id(), OsStr::new("foo"))
            .await;

        assert_eq!(hardlink_res.is_err(), true);
        let err: Errno = libc::EEXIST.into();
        assert_eq!(hardlink_res.err().unwrap(), err);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_hardlink_fails_when_object_is_directory() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let mkdir_reply = fs
            .mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), 0, DEFAULT_FLAG)
            .await
            .expect("mkdir failed");

        let hardlink_res = fs
            .link(new_fake_request(), mkdir_reply.attr.ino, dir.object_id(), OsStr::new("bar"))
            .await;

        assert_eq!(hardlink_res.is_err(), true);
        let err: Errno = libc::EISDIR.into();
        assert_eq!(hardlink_res.err().unwrap(), err);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_hardlink_fails_when_parent_is_not_directory() {
        let fs = FuseFs::new_in_memory().await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("foo"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create_reply failed");

        let _create_reply = fs
            .create(
                new_fake_request(),
                dir.object_id(),
                OsStr::new("bar"),
                DEFAULT_FILE_MODE,
                DEFAULT_FLAG,
            )
            .await
            .expect("create_reply failed");

        let hardlink_res = fs
            .link(
                new_fake_request(),
                create_reply.attr.ino,
                _create_reply.attr.ino,
                OsStr::new("link"),
            )
            .await;

        assert_eq!(hardlink_res.is_err(), true);
        let err: Errno = libc::ENOTDIR.into();
        assert_eq!(hardlink_res.err().unwrap(), err);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_mkdir_create_directory_tree_and_reopen() {
        let fs = FuseFs::new_file_backed("/tmp/fuse_dir_test").await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let mkdir_reply = fs
            .mkdir(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("mkdir failed");
        let (child_id, child_descriptor) =
            dir.lookup(OsStr::new("foo").parse_str().unwrap()).await.unwrap().unwrap();
        assert_eq!(child_descriptor, ObjectDescriptor::Directory);
        assert_eq!(mkdir_reply.attr.ino, child_id);
        assert_eq!(mkdir_reply.attr.kind, FileType::Directory);

        fs.destroy(new_fake_request()).await;

        let fs = FuseFs::open_file_backed("/tmp/fuse_dir_test").await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let (_child_id, _child_descriptor) =
            dir.lookup(OsStr::new("foo").parse_str().unwrap()).await.unwrap().unwrap();
        assert_eq!(_child_descriptor, ObjectDescriptor::Directory);

        fs.fs.close().await.expect("failed to close filesystem");
    }

    #[fuchsia::test]
    async fn test_mkdir_create_file_and_reopen() {
        let fs = FuseFs::new_file_backed("/tmp/fuse_file_test").await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let create_reply = fs
            .create(new_fake_request(), dir.object_id(), OsStr::new("foo"), DEFAULT_FILE_MODE, 0)
            .await
            .expect("open_file failed");
        let (child_id, child_descriptor) =
            dir.lookup(OsStr::new("foo").parse_str().unwrap()).await.unwrap().unwrap();
        assert_eq!(child_descriptor, ObjectDescriptor::File);
        assert_eq!(create_reply.attr.ino, child_id);
        assert_eq!(create_reply.attr.kind, FileType::RegularFile);

        fs.destroy(new_fake_request()).await;

        let fs = FuseFs::open_file_backed("/tmp/fuse_file_test").await;
        let dir = fs.root_dir().await.expect("root_dir failed");

        let (_child_id, _child_descriptor) =
            dir.lookup(OsStr::new("foo").parse_str().unwrap()).await.unwrap().unwrap();
        assert_eq!(_child_descriptor, ObjectDescriptor::File);

        fs.fs.close().await.expect("failed to close filesystem");
    }
}
