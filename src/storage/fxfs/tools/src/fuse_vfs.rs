// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
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
        object_handle::ObjectHandle,
        object_store::{
            transaction::{Options, TransactionHandler},
            ObjectDescriptor,
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

    /// Initializes filesystem. Called before any other filesystem method.
    async fn init(&self, _req: Request) -> Result<()> {
        info!("init");
        Ok(())
    }

    /// Gracefully closes filesystem. Currently fuse3 cannot handle unmount gracefully,
    /// see https://github.com/Sherlock-Holo/fuse3/issues/52.
    /// To fix it, destroy() is triggered when SIGTERM is received by the FUSE-Fxfs program.
    async fn destroy(&self, _req: Request) {
        info!("destroy");
        self.fs.close().await.expect("close failed");
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
}

#[cfg(test)]
mod tests {
    use {
        crate::fuse_fs::{FuseFs, FuseStrParser},
        fuse3::{
            raw::{Filesystem, Request},
            FileType,
        },
        fxfs::object_store::ObjectDescriptor,
        std::ffi::OsStr,
    };

    const DEFAULT_FILE_MODE: u32 = 0o755;
    const DEFAULT_FLAG: u32 = 0;
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
    async fn test_create_fails_when_parent_does_not_exists() {
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
    async fn test_open_fails_when_file_does_not_exists() {
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
