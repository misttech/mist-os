// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    assert_matches::assert_matches,
    async_trait::async_trait,
    fidl::endpoints::create_endpoints,
    fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_zircon::{self as zx, HandleBased},
    fxfs_testing::TestFixture,
    std::sync::Arc,
    syncio::{
        zxio, zxio_node_attr_has_t, zxio_node_attributes_t, OpenOptions, SeekOrigin, XattrSetMode,
        Zxio,
    },
    vfs::{
        directory::entry::DirectoryEntry, execution_scope::ExecutionScope, node::Node, path::Path,
        pseudo_directory, symlink::Symlink,
    },
};

#[fuchsia::test]
async fn test_symlink() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, dir_server.into())
        .expect("clone failed");

    fasync::unblock(|| {
        let dir_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        let symlink_zxio =
            dir_zxio.create_symlink("symlink", b"target").expect("create_symlink failed");
        assert_eq!(symlink_zxio.read_link().expect("read_link failed"), b"target");

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
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_read_link_error() {
    struct ErrorSymlink;

    #[async_trait]
    impl Symlink for ErrorSymlink {
        async fn read_target(&self) -> Result<Vec<u8>, zx::Status> {
            Err(zx::Status::IO)
        }
    }

    #[async_trait]
    impl Node for ErrorSymlink {
        async fn get_attributes(
            &self,
            _requested_attributes: fio::NodeAttributesQuery,
        ) -> Result<fio::NodeAttributes2, zx::Status> {
            unreachable!();
        }

        async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
            unreachable!();
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

    fasync::unblock(|| {
        let dir_zxio =
            Zxio::create(dir_client.into_channel().into_handle()).expect("create failed");

        assert_eq!(
            dir_zxio
                .open(fio::OpenFlags::RIGHT_READABLE, "error_symlink")
                .expect_err("open succeeded"),
            zx::Status::IO
        );
    })
    .await;
}

#[fuchsia::test]
async fn test_xattr_dir() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .open(
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY
                | fio::OpenFlags::CREATE,
            fio::ModeType::empty(),
            "foo",
            dir_server.into(),
        )
        .expect("open failed");

    fasync::unblock(|| {
        let foo_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        assert_matches!(foo_zxio.xattr_get(b"security.selinux"), Err(zx::Status::NOT_FOUND));

        foo_zxio.xattr_set(b"security.selinux", b"bar", XattrSetMode::Set).unwrap();

        assert_eq!(foo_zxio.xattr_get(b"security.selinux").unwrap(), b"bar");

        {
            let names = foo_zxio.xattr_list().unwrap();
            assert_eq!(names, vec![b"security.selinux".to_owned()]);
        }

        foo_zxio.xattr_remove(b"security.selinux").unwrap();

        assert_matches!(foo_zxio.xattr_get(b"security.selinux"), Err(zx::Status::NOT_FOUND));

        {
            let names = foo_zxio.xattr_list().unwrap();
            assert_eq!(names, Vec::<Vec<u8>>::new());
        }
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_xattr_dir_multiple_attributes() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .open(
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY
                | fio::OpenFlags::CREATE,
            fio::ModeType::empty(),
            "foo",
            dir_server.into(),
        )
        .expect("open failed");

    fasync::unblock(|| {
        let foo_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        let names = &[
            b"security.selinux".to_vec(),
            b"user.sha".to_vec(),
            b"fuchsia.merkle".to_vec(),
            b"starnix.mode".to_vec(),
            b"invalid in linux but fine for fuchsia!".to_vec(),
        ];
        let mut sorted_names = names.to_vec();
        sorted_names.sort();
        let values = &[
            b"important security attribute".to_vec(),
            b"abc1234".to_vec(),
            b"fffffffffff".to_vec(),
            b"drwxrwxrwx".to_vec(),
            b"\0\0 nulls are fine in the value \0\0\0".to_vec(),
        ];

        for (name, value) in names.iter().zip(values.iter()) {
            foo_zxio.xattr_set(&name, &value, XattrSetMode::Set).unwrap();
        }

        let mut listed_names = foo_zxio.xattr_list().unwrap();
        listed_names.sort();
        // Sort the two lists, because there isn't any guaranteed order they will come back from the
        // server.
        assert_eq!(listed_names, sorted_names);
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_xattr_symlink() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, dir_server.into())
        .expect("clone failed");

    fasync::unblock(|| {
        let dir_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        let symlink_zxio =
            dir_zxio.create_symlink("symlink", b"target").expect("create_symlink failed");
        assert_eq!(symlink_zxio.read_link().expect("read_link failed"), b"target");

        assert_matches!(symlink_zxio.xattr_get(b"security.selinux"), Err(zx::Status::NOT_FOUND));

        symlink_zxio.xattr_set(b"security.selinux", b"bar", XattrSetMode::Set).unwrap();

        assert_eq!(symlink_zxio.xattr_get(b"security.selinux").unwrap(), b"bar");

        {
            let names = symlink_zxio.xattr_list().unwrap();
            assert_eq!(names, vec![b"security.selinux".to_owned()]);
        }

        symlink_zxio.xattr_remove(b"security.selinux").unwrap();

        assert_matches!(symlink_zxio.xattr_get(b"security.selinux"), Err(zx::Status::NOT_FOUND));

        {
            let names = symlink_zxio.xattr_list().unwrap();
            assert_eq!(names, Vec::<Vec<u8>>::new());
        }
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_xattr_file_large_attribute() {
    let fixture = TestFixture::new().await;

    let (foo_client, foo_server) = zx::Channel::create();
    fixture
        .root()
        .open(
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::CREATE,
            fio::ModeType::empty(),
            "foo",
            foo_server.into(),
        )
        .expect("open failed");

    fasync::unblock(|| {
        let foo_zxio = Zxio::create(foo_client.into_handle()).expect("create failed");

        let value_len = fio::MAX_INLINE_ATTRIBUTE_VALUE as usize + 64;
        let value = std::iter::repeat(0xff).take(value_len).collect::<Vec<u8>>();
        foo_zxio.xattr_set(b"user.big_attribute", &value, XattrSetMode::Set).unwrap();

        assert_eq!(foo_zxio.xattr_get(b"user.big_attribute").unwrap(), value);
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_open2() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, dir_server.into())
        .expect("clone failed");

    fasync::unblock(|| {
        let dir_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        // Create a test directory.
        let test_dir = dir_zxio
            .open2(
                "test_dir",
                OpenOptions { mode: fio::OpenMode::AlwaysCreate, ..OpenOptions::directory(None) },
                None,
            )
            .expect("open2 failed");

        // Now create a file in that directory.
        let test_file = test_dir
            .open2(
                "test_file",
                OpenOptions {
                    mode: fio::OpenMode::AlwaysCreate,
                    ..OpenOptions::file(fio::FileProtocolFlags::empty())
                },
                None,
            )
            .expect("open2 failed");

        // Write something to the file.
        const CONTENT: &[u8] = b"hello";
        test_file.write(CONTENT).expect("write failed");

        // Open the directory without specifying any protocols and ask for attributes.
        let mut attr = zxio_node_attributes_t {
            has: zxio_node_attr_has_t {
                protocols: true,
                abilities: true,
                id: true,
                link_count: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let test_dir = dir_zxio
            .open2("test_dir", OpenOptions::default(), Some(&mut attr))
            .expect("open2 failed");

        assert_eq!(attr.protocols, zxio::ZXIO_NODE_PROTOCOL_DIRECTORY);
        assert_eq!(
            attr.abilities,
            (fio::Operations::ENUMERATE
                | fio::Operations::TRAVERSE
                | fio::Operations::MODIFY_DIRECTORY
                | fio::Operations::GET_ATTRIBUTES
                | fio::Operations::UPDATE_ATTRIBUTES)
                .bits()
        );
        // Fxfs will always return a non-zero ID.
        assert_ne!(attr.id, 0);
        assert_eq!(attr.link_count, 2);

        // And now the file, and ask for attributes.
        let mut attr = zxio_node_attributes_t {
            has: zxio_node_attr_has_t {
                protocols: true,
                abilities: true,
                id: true,
                content_size: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let _test_file = test_dir
            .open2("test_file", OpenOptions::default(), Some(&mut attr))
            .expect("open2 failed");
        assert_eq!(attr.protocols, zxio::ZXIO_NODE_PROTOCOL_FILE);
        assert_eq!(
            attr.abilities,
            (fio::Operations::READ_BYTES
                | fio::Operations::WRITE_BYTES
                | fio::Operations::GET_ATTRIBUTES
                | fio::Operations::UPDATE_ATTRIBUTES)
                .bits()
        );
        // Fxfs will always return a non-zero ID.
        assert_ne!(attr.id, 0);
        assert_eq!(attr.content_size, 5);

        // Create and open a symlink.
        test_dir.create_symlink("symlink", b"target").expect("create_symlink failed");

        let symlink =
            test_dir.open2("symlink", OpenOptions::default(), None).expect("open2 failed");
        assert_eq!(symlink.read_link().expect("read_link failed"), b"target");

        // Test file protocol flags.
        let test_file = test_dir
            .open2("test_file", OpenOptions::file(fio::FileProtocolFlags::APPEND), None)
            .expect("open2 failed");
        const APPEND_CONTENT: &[u8] = b" there";
        test_file.write(APPEND_CONTENT).expect("write failed");
        test_file.seek(SeekOrigin::Start, 0).expect("seek failed");
        let mut buf = [0; 20];
        assert_eq!(
            test_file.read(&mut buf).expect("read failed"),
            CONTENT.len() + APPEND_CONTENT.len()
        );
        assert_eq!(&buf[..CONTENT.len()], CONTENT);
        assert_eq!(&buf[CONTENT.len()..CONTENT.len() + APPEND_CONTENT.len()], APPEND_CONTENT);

        // TODO(fxbug.dev/127341): Test create attributes.
        // TODO(fxbug.dev/125830): Test for POSIX attributes.
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_open2_rights() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, dir_server.into())
        .expect("clone failed");

    fasync::unblock(|| {
        let dir_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        // Create and write to a file.
        let test_file = dir_zxio
            .open2(
                "test_file",
                OpenOptions {
                    mode: fio::OpenMode::AlwaysCreate,
                    ..OpenOptions::file(fio::FileProtocolFlags::empty())
                },
                None,
            )
            .expect("open2 failed");

        // Write something to the file.
        const CONTENT: &[u8] = b"hello";
        test_file.write(CONTENT).expect("write failed");

        // Check rights
        let test_file = dir_zxio
            .open2(
                "test_file",
                OpenOptions { rights: fio::Operations::READ_BYTES, ..Default::default() },
                None,
            )
            .expect("open2 failed");
        let mut buf = [0; 20];
        assert_eq!(test_file.read(&mut buf).expect("read failed"), CONTENT.len());
        assert_eq!(&buf[..CONTENT.len()], CONTENT);

        // Make sure we can't write to the file.
        assert_eq!(test_file.write(&buf).expect_err("write succeeded"), zx::Status::BAD_HANDLE);

        // Check rights inherited from directory.
        let dir = dir_zxio
            .open2(
                ".",
                OpenOptions { rights: fio::Operations::READ_BYTES, ..Default::default() },
                None,
            )
            .expect("open2 failed");

        let test_file = dir.open2("test_file", OpenOptions::default(), None).expect("open2 failed");

        assert_eq!(test_file.read(&mut buf).expect("read failed"), CONTENT.len());
        assert_eq!(&buf[..CONTENT.len()], CONTENT);

        // Make sure we can't write to the file.
        assert_eq!(test_file.write(&buf).expect_err("write succeeded"), zx::Status::BAD_HANDLE);

        // Optional rights on directories.
        let test_dir = dir_zxio
            .open2(
                ".",
                OpenOptions {
                    rights: fio::Operations::READ_BYTES,
                    ..OpenOptions::directory(Some(fio::Operations::WRITE_BYTES))
                },
                None,
            )
            .expect("open2 failed");

        // Now make sure we can open and write to the test file.
        let test_file =
            test_dir.open2("test_file", OpenOptions::default(), None).expect("open2 failed");

        assert_eq!(test_file.read(&mut buf).expect("read failed"), CONTENT.len());
        assert_eq!(&buf[..CONTENT.len()], CONTENT);

        // This time we should be able to write to the file.
        test_file.write(b"foo").expect("write failed");
    })
    .await;

    fixture.close().await;
}

#[fuchsia::test]
async fn test_open2_node() {
    let fixture = TestFixture::new().await;

    let (dir_client, dir_server) = zx::Channel::create();
    fixture
        .root()
        .clone(fio::OpenFlags::CLONE_SAME_RIGHTS, dir_server.into())
        .expect("clone failed");

    fasync::unblock(|| {
        let dir_zxio = Zxio::create(dir_client.into_handle()).expect("create failed");

        dir_zxio
            .open_node(".", fio::NodeProtocolFlags::MUST_BE_DIRECTORY, None)
            .expect("open_node failed");

        // Create a file.
        let test_file = dir_zxio
            .open2(
                "test_file",
                OpenOptions {
                    mode: fio::OpenMode::AlwaysCreate,
                    ..OpenOptions::file(fio::FileProtocolFlags::empty())
                },
                None,
            )
            .expect("open2 failed");

        // Write something to the file.
        const CONTENT: &[u8] = b"hello";
        test_file.write(CONTENT).expect("write failed");

        assert_eq!(
            dir_zxio
                .open_node("test_file", fio::NodeProtocolFlags::MUST_BE_DIRECTORY, None,)
                .expect_err("open_node succeeded"),
            zx::Status::NOT_DIR
        );

        // Check we can get attributes.
        let mut attr = zxio_node_attributes_t {
            has: zxio_node_attr_has_t { protocols: true, content_size: true, ..Default::default() },
            ..Default::default()
        };

        dir_zxio
            .open_node("test_file", fio::NodeProtocolFlags::empty(), Some(&mut attr))
            .expect("open_node failed");

        assert_eq!(attr.protocols, zxio::ZXIO_NODE_PROTOCOL_FILE);
        assert_eq!(attr.content_size, 5);
    })
    .await;

    fixture.close().await;
}
