// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_trait::async_trait,
    fidl::endpoints::create_endpoints,
    fidl_fuchsia_io as fio,
    fuchsia_zircon::{self as zx, HandleBased},
    fxfs_testing::TestFixture,
    std::sync::Arc,
    syncio::Zxio,
    vfs::{
        directory::entry::DirectoryEntry, execution_scope::ExecutionScope, path::Path,
        pseudo_directory, symlink::Symlink,
    },
};

// Some tests here need more than one thread because zxio makes synchronous FIDL calls.

#[fuchsia::test(threads = 2)]
async fn test_symlink() {
    let fixture = TestFixture::new().await;

    {
        let (dir_client, dir_server) = zx::Channel::create();
        fixture
            .root()
            .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, dir_server.into())
            .expect("clone failed");

        let dir_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        dir_zxio.create_symlink("symlink", b"target").expect("create_symlink failed");

        // Test some error cases
        assert_eq!(
            dir_zxio.create_symlink("symlink", b"target").expect_err("create symlink succeeded"),
            zx::Status::ALREADY_EXISTS
        );
        assert_eq!(
            dir_zxio.create_symlink("a/b", b"target").expect_err("create symlink succeeded"),
            zx::Status::INVALID_ARGS
        );
        assert_eq!(
            dir_zxio
                .create_symlink("symlink2", &vec![65; 300])
                .expect_err("create symlink succeeded"),
            zx::Status::BAD_PATH
        );

        let symlink_zxio =
            dir_zxio.open(fio::OpenFlags::RIGHT_READABLE, "symlink").expect("open failed");
        assert_eq!(symlink_zxio.read_link().expect("read_link failed"), b"target");
    }

    fixture.close().await;
}

#[fuchsia::test(threads = 2)]
async fn test_read_link_error() {
    struct ErrorSymlink;

    #[async_trait]
    impl Symlink for ErrorSymlink {
        async fn read_target(&self) -> Result<Vec<u8>, zx::Status> {
            Err(zx::Status::IO)
        }
    }

    let dir = pseudo_directory! {
        "error_symlink" => Arc::new(ErrorSymlink),
    };

    let (dir_client, dir_server) = create_endpoints();
    let scope = ExecutionScope::new();
    dir.open(
        scope,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
        Path::dot(),
        dir_server,
    );

    let dir_zxio = Zxio::create(dir_client.into_channel().into_handle()).expect("create failed");

    assert_eq!(
        dir_zxio.open(fio::OpenFlags::RIGHT_READABLE, "error_symlink").expect_err("open succeeded"),
        zx::Status::IO
    );
}
