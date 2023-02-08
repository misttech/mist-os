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
        log::info,
        object_handle::{ObjectHandle, WriteObjectHandle},
        object_store::{
            transaction::{Options, TransactionHandler},
            ObjectDescriptor, Timestamp,
        },
    },
    std::{ffi::OsStr, time::Duration, vec::IntoIter},
};

/// The TTL duration of each reply.
const TTL: Duration = Duration::from_secs(1);

/// In the function parameters, request, mode and mask are not
/// supported by Fxfs and hence they are ignored.
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
        fxfs::object_store::ObjectDescriptor,
        std::ffi::OsStr,
    };

    const DEFAULT_FILE_MODE: u32 = 0o755;
    const DEFAULT_FLAG: u32 = 0;
    const DEFAULT_TIME: Timestamp = Timestamp { sec: 10i64, nsec: 20u32 };
    const INVALID_INODE: u64 = 0;

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
