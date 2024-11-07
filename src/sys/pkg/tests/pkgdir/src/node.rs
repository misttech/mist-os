// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{dirs_to_test, PackageSource};
use anyhow::{anyhow, Context as _, Error};
use fidl::endpoints::Proxy as _;
use fidl::AsHandleRef as _;
use fidl_fuchsia_io as fio;

#[fuchsia::test]
async fn get_attributes() {
    for source in dirs_to_test().await {
        get_attributes_per_package_source(source).await
    }
}

trait U64Verifier: std::fmt::Debug {
    #[track_caller]
    fn verify(&self, num: u64);
}

impl U64Verifier for u64 {
    fn verify(&self, num: u64) {
        assert_eq!(num, *self)
    }
}

#[derive(Debug)]
struct AnyU64;
impl U64Verifier for AnyU64 {
    fn verify(&self, _num: u64) {}
}

async fn get_attributes_per_package_source(source: PackageSource) {
    let root_dir = &source.dir;
    #[derive(Debug)]
    struct Args {
        open_flags: fio::Flags,
        expected_protocols: fio::NodeProtocolKinds,
        expected_abilities: fio::Abilities,
        id_verifier: Option<Box<dyn U64Verifier>>,
        expected_content_size: Option<u64>,
        storage_size_verifier: Option<Box<dyn U64Verifier>>,
    }

    impl Default for Args {
        fn default() -> Self {
            Self {
                open_flags: Default::default(),
                expected_protocols: Default::default(),
                expected_abilities: Default::default(),
                id_verifier: Some(Box::new(1)),
                expected_content_size: None,
                storage_size_verifier: None,
            }
        }
    }

    async fn verify_get_attributes(root_dir: &fio::DirectoryProxy, path: &str, args: Args) {
        let node = fuchsia_fs::directory::open_node(root_dir, path, args.open_flags).await.unwrap();
        let (_, immut_attrs) = node
            .get_attributes(fio::NodeAttributesQuery::all())
            .await
            .unwrap()
            .expect("fuchsia.io/Node.GetAttributes failed");
        assert_eq!(immut_attrs.protocols.unwrap(), args.expected_protocols);
        assert_eq!(immut_attrs.abilities.unwrap(), args.expected_abilities);
        if let Some(verifier) = args.id_verifier {
            verifier.verify(immut_attrs.id.unwrap());
        }
        assert_eq!(immut_attrs.content_size, args.expected_content_size);
        if let Some(verifier) = args.storage_size_verifier {
            verifier.verify(immut_attrs.storage_size.unwrap());
        } else {
            assert!(immut_attrs.storage_size.is_none());
        }
    }

    verify_get_attributes(
        root_dir,
        ".",
        Args {
            open_flags: fuchsia_fs::PERM_READABLE | fuchsia_fs::PERM_EXECUTABLE,
            expected_protocols: fio::NodeProtocolKinds::DIRECTORY,
            expected_abilities: fio::Abilities::GET_ATTRIBUTES
                | fio::Abilities::ENUMERATE
                | fio::Abilities::TRAVERSE,
            ..Default::default()
        },
    )
    .await;
    verify_get_attributes(
        root_dir,
        "dir",
        Args {
            expected_protocols: fio::NodeProtocolKinds::DIRECTORY,
            expected_abilities: fio::Abilities::GET_ATTRIBUTES
                | fio::Abilities::ENUMERATE
                | fio::Abilities::TRAVERSE,
            ..Default::default()
        },
    )
    .await;
    verify_get_attributes(
        root_dir,
        "file",
        Args {
            open_flags: fuchsia_fs::PERM_READABLE,
            expected_protocols: fio::NodeProtocolKinds::FILE,
            expected_abilities: fio::Abilities::GET_ATTRIBUTES
                | fio::Abilities::READ_BYTES
                | fio::Abilities::EXECUTE,
            id_verifier: None,
            expected_content_size: Some(4),
            storage_size_verifier: Some(Box::new(AnyU64)),
        },
    )
    .await;
    verify_get_attributes(
        root_dir,
        "meta",
        Args {
            open_flags: fio::Flags::PROTOCOL_FILE,
            expected_protocols: fio::NodeProtocolKinds::FILE,
            expected_abilities: fio::Abilities::GET_ATTRIBUTES | fio::Abilities::READ_BYTES,
            expected_content_size: Some(64),
            storage_size_verifier: Some(Box::new(AnyU64)),
            ..Default::default()
        },
    )
    .await;
    verify_get_attributes(
        root_dir,
        "meta",
        Args {
            open_flags: fio::Flags::PROTOCOL_DIRECTORY,
            expected_protocols: fio::NodeProtocolKinds::DIRECTORY,
            expected_abilities: fio::Abilities::GET_ATTRIBUTES
                | fio::Abilities::ENUMERATE
                | fio::Abilities::TRAVERSE,
            expected_content_size: Some(75),
            storage_size_verifier: Some(Box::new(AnyU64)),
            ..Default::default()
        },
    )
    .await;
    verify_get_attributes(
        root_dir,
        "meta/dir",
        Args {
            expected_protocols: fio::NodeProtocolKinds::DIRECTORY,
            expected_abilities: fio::Abilities::GET_ATTRIBUTES
                | fio::Abilities::ENUMERATE
                | fio::Abilities::TRAVERSE,
            expected_content_size: Some(75),
            storage_size_verifier: Some(Box::new(AnyU64)),
            ..Default::default()
        },
    )
    .await;
    verify_get_attributes(
        root_dir,
        "meta/file",
        Args {
            expected_protocols: fio::NodeProtocolKinds::FILE,
            expected_abilities: fio::Abilities::GET_ATTRIBUTES | fio::Abilities::READ_BYTES,
            expected_content_size: Some(9),
            storage_size_verifier: Some(Box::new(AnyU64)),
            ..Default::default()
        },
    )
    .await;
}

#[fuchsia::test]
async fn close() {
    for source in dirs_to_test().await {
        close_per_package_source(source).await
    }
}

async fn close_per_package_source(source: PackageSource) {
    let root_dir = source.dir;
    async fn verify_close(root_dir: &fio::DirectoryProxy, path: &str, flags: fio::Flags) {
        let node =
            fuchsia_fs::directory::open_node(root_dir, path, flags | fuchsia_fs::PERM_READABLE)
                .await
                .unwrap();

        let () = node.close().await.unwrap().map_err(zx::Status::from_raw).unwrap();

        assert_matches::assert_matches!(
            node.close().await,
            Err(fidl::Error::ClientChannelClosed { status: zx::Status::PEER_CLOSED, .. })
        );
    }

    verify_close(&root_dir, ".", fio::Flags::PROTOCOL_DIRECTORY).await;
    verify_close(&root_dir, "dir", fio::Flags::PROTOCOL_DIRECTORY).await;
    verify_close(&root_dir, "meta", fio::Flags::PROTOCOL_DIRECTORY).await;
    verify_close(&root_dir, "meta/dir", fio::Flags::PROTOCOL_DIRECTORY).await;

    verify_close(&root_dir, "file", fio::Flags::PROTOCOL_FILE).await;
    verify_close(&root_dir, "meta/file", fio::Flags::PROTOCOL_FILE).await;
    verify_close(&root_dir, "meta", fio::Flags::PROTOCOL_FILE).await;
}

#[fuchsia::test]
async fn describe() {
    for source in dirs_to_test().await {
        describe_per_package_source(source).await
    }
}

async fn describe_per_package_source(source: PackageSource) {
    let root_dir = source.dir;
    assert_query_directory(&root_dir, ".").await;

    assert_query_directory(&root_dir, "meta").await;
    assert_describe_meta_file(&root_dir, "meta").await;

    assert_query_directory(&root_dir, "meta/dir").await;
    assert_query_directory(&root_dir, "dir").await;

    assert_describe_file(&root_dir, "file").await;
    assert_describe_meta_file(&root_dir, "meta/file").await;
}

async fn assert_query_directory(package_root: &fio::DirectoryProxy, path: &str) {
    for flag in [fio::Flags::empty(), fio::Flags::PROTOCOL_NODE] {
        let node = fuchsia_fs::directory::open_node(
            package_root,
            path,
            flag | fio::Flags::PROTOCOL_DIRECTORY,
        )
        .await
        .unwrap();

        if let Err(e) = verify_query_directory(node, flag).await {
            panic!(
                "failed to verify describe. path: {path:?}, flag: {flag:?}, \
                    error: {e:#}",
            );
        }
    }
}

async fn verify_query_directory(node: fio::NodeProxy, flag: fio::Flags) -> Result<(), Error> {
    let protocol = String::from_utf8(node.query().await.context("failed to call query")?).unwrap();
    let expected = if flag.intersects(fio::Flags::PROTOCOL_NODE) {
        crate::NODE_PROTOCOL_NAMES
    } else {
        crate::DIRECTORY_PROTOCOL_NAMES
    };
    if expected.contains(&protocol.as_str()) {
        Ok(())
    } else {
        Err(anyhow!("wrong protocol returned: {:?}", protocol))
    }
}

async fn assert_describe_file(package_root: &fio::DirectoryProxy, path: &str) {
    for flag in [fuchsia_fs::PERM_READABLE, fio::Flags::PROTOCOL_NODE] {
        let node = fuchsia_fs::directory::open_node(package_root, path, flag).await.unwrap();
        if let Err(e) = verify_describe_file(node, flag).await {
            panic!(
                "failed to verify describe. path: {path:?}, flag: {flag:?}, \
                    error: {e:#}",
            );
        }
    }
}

async fn verify_describe_file(node: fio::NodeProxy, flag: fio::Flags) -> Result<(), Error> {
    let protocol = String::from_utf8(node.query().await.context("failed to call query")?).unwrap();
    if flag.intersects(fio::Flags::PROTOCOL_NODE) {
        if crate::NODE_PROTOCOL_NAMES.contains(&protocol.as_str()) {
            Ok(())
        } else {
            Err(anyhow!("wrong protocol returned: {:?}", protocol))
        }
    } else if crate::FILE_PROTOCOL_NAMES.contains(&protocol.as_str()) {
        let fio::FileInfo { observer, .. } = fio::FileProxy::new(node.into_channel().unwrap())
            .describe()
            .await
            .context("failed to call describe")?;
        // Only blobfs blobs set the observer to indicate when the blob is readable. The blobs
        // should be immediately readable here.
        if let Some(observer) = observer {
            let _: zx::Signals = observer
                .wait_handle(zx::Signals::USER_0, zx::MonotonicInstant::INFINITE_PAST)
                .context("FILE_SIGNAL_READABLE not set")?;
        }
        // TODO(https://fxbug.dev/327633753): Check for stream support.
        Ok(())
    } else {
        Err(anyhow!("wrong protocol returned: {:?}", protocol))
    }
}

async fn assert_describe_meta_file(package_root: &fio::DirectoryProxy, path: &str) {
    for flag in [fio::Flags::empty(), fio::Flags::PROTOCOL_NODE] {
        let node = fuchsia_fs::directory::open_node(package_root, path, flag).await.unwrap();
        if let Err(e) = verify_describe_file(node, flag).await {
            panic!(
                "failed to verify describe. path: {path:?}, flag: {flag:?}, \
                    error: {e:#}"
            );
        }
    }
}

#[fuchsia::test]
async fn set_flags() {
    for source in dirs_to_test().await {
        set_flags_per_package_source(source).await
    }
}

async fn set_flags_per_package_source(source: PackageSource) {
    let package_root = &source.dir;
    do_set_flags(package_root, ".", fio::Flags::PROTOCOL_DIRECTORY, fio::OpenFlags::empty())
        .await
        .assert_not_supported();
    do_set_flags(package_root, "meta", fio::Flags::PROTOCOL_DIRECTORY, fio::OpenFlags::empty())
        .await
        .assert_not_supported();
    do_set_flags(package_root, "meta/dir", fio::Flags::PROTOCOL_DIRECTORY, fio::OpenFlags::empty())
        .await
        .assert_not_supported();
    do_set_flags(package_root, "dir", fio::Flags::PROTOCOL_DIRECTORY, fio::OpenFlags::empty())
        .await
        .assert_not_supported();
    do_set_flags(package_root, "file", fio::Flags::PROTOCOL_FILE, fio::OpenFlags::empty())
        .await
        .assert_ok();
    do_set_flags(package_root, "meta", fio::Flags::PROTOCOL_FILE, fio::OpenFlags::empty())
        .await
        .assert_ok();
    do_set_flags(package_root, "meta", fio::Flags::PROTOCOL_FILE, fio::OpenFlags::APPEND)
        .await
        .assert_ok();
    do_set_flags(package_root, "meta/file", fio::Flags::PROTOCOL_FILE, fio::OpenFlags::empty())
        .await
        .assert_ok();
    do_set_flags(package_root, "meta/file", fio::Flags::PROTOCOL_FILE, fio::OpenFlags::APPEND)
        .await
        .assert_ok();
}

struct SetFlagsOutcome<'a> {
    argument: fio::OpenFlags,
    path: &'a str,
    result: Result<Result<(), zx::Status>, fidl::Error>,
}
async fn do_set_flags<'a>(
    package_root: &fio::DirectoryProxy,
    path: &'a str,
    flags: fio::Flags,
    argument: fio::OpenFlags,
) -> SetFlagsOutcome<'a> {
    let node =
        fuchsia_fs::directory::open_node(package_root, path, flags | fuchsia_fs::PERM_READABLE)
            .await
            .unwrap();

    let result = node.set_flags(argument).await.map(zx::Status::ok);
    SetFlagsOutcome { path, result, argument }
}

impl SetFlagsOutcome<'_> {
    fn error_context(&self) -> String {
        format!("path: {:?}", self.path)
    }

    #[track_caller]
    fn assert_not_supported(&self) {
        match &self.result {
            Ok(Err(zx::Status::NOT_SUPPORTED)) => {}
            Ok(Err(e)) => panic!(
                "set_flags({:?}): wrong error status: {} on {}",
                self.argument,
                e,
                self.error_context()
            ),
            Err(e) => panic!(
                "failed to call set_flags({:?}): {:?} on {}",
                self.argument,
                e,
                self.error_context()
            ),
            Ok(Ok(())) => panic!(
                "set_flags({:?}) succeeded unexpectedly on {}",
                self.argument,
                self.error_context()
            ),
        };
    }

    #[track_caller]
    fn assert_ok(&self) {
        match &self.result {
            Ok(Ok(())) => {}
            e => {
                panic!("set_flags({:?}) failed: {:?} on {}", self.argument, e, self.error_context())
            }
        }
    }
}

#[fuchsia::test]
async fn set_attr() {
    for source in dirs_to_test().await {
        set_attr_per_package_source(source).await
    }
}

async fn set_attr_per_package_source(source: PackageSource) {
    let root_dir = source.dir;
    assert_set_attr(&root_dir, ".", fio::Flags::PROTOCOL_DIRECTORY).await;
    assert_set_attr(&root_dir, "meta", fio::Flags::PROTOCOL_DIRECTORY).await;
    assert_set_attr(&root_dir, "meta/dir", fio::Flags::PROTOCOL_DIRECTORY).await;
    assert_set_attr(&root_dir, "dir", fio::Flags::PROTOCOL_DIRECTORY).await;
    assert_set_attr(&root_dir, "meta", fio::Flags::PROTOCOL_FILE).await;
    assert_set_attr(&root_dir, "meta/file", fio::Flags::PROTOCOL_FILE).await;
}

async fn assert_set_attr(package_root: &fio::DirectoryProxy, path: &str, flags: fio::Flags) {
    let node = fuchsia_fs::directory::open_node(package_root, path, flags).await.unwrap();

    if let Err(e) = verify_set_attr(node).await {
        panic!("set_attr failed. path: {path:?}, error: {e:#}");
    }
}

async fn verify_set_attr(node: fio::NodeProxy) -> Result<(), Error> {
    let node_attr = fio::NodeAttributes {
        mode: 0,
        id: 0,
        content_size: 0,
        storage_size: 0,
        link_count: 0,
        creation_time: 0,
        modification_time: 0,
    };
    match node.set_attr(fio::NodeAttributeFlags::empty(), &node_attr).await {
        Ok(status) => {
            if matches!(
                zx::Status::from_raw(status),
                zx::Status::NOT_SUPPORTED | zx::Status::BAD_HANDLE
            ) {
                Ok(())
            } else {
                Err(anyhow!("wrong status returned: {:?}", zx::Status::from_raw(status)))
            }
        }
        Err(e) => Err(e).context("failed to call set_attr"),
    }
}

#[fuchsia::test]
async fn sync() {
    for source in dirs_to_test().await {
        sync_per_package_source(source).await
    }
}

async fn sync_per_package_source(source: PackageSource) {
    let root_dir = source.dir;
    assert_sync(&root_dir, ".", fio::Flags::PROTOCOL_DIRECTORY).await;
    assert_sync(&root_dir, "meta", fio::Flags::PROTOCOL_DIRECTORY).await;
    assert_sync(&root_dir, "meta/dir", fio::Flags::PROTOCOL_DIRECTORY).await;
    assert_sync(&root_dir, "dir", fio::Flags::PROTOCOL_DIRECTORY).await;
    assert_sync(&root_dir, "meta", fio::Flags::PROTOCOL_FILE).await;
    assert_sync(&root_dir, "meta/file", fio::Flags::PROTOCOL_FILE).await;
}

async fn assert_sync(package_root: &fio::DirectoryProxy, path: &str, flags: fio::Flags) {
    let node = fuchsia_fs::directory::open_node(package_root, path, flags).await.unwrap();

    if let Err(e) = verify_sync(node).await {
        panic!("sync failed. path: {path:?}, error: {e:#}");
    }
}

async fn verify_sync(node: fio::NodeProxy) -> Result<(), Error> {
    let result = node.sync().await.context("failed to call sync")?;
    let result = result.map_err(zx::Status::from_raw);
    // All of the files and directories are immutable so it's valid to return either success or
    // NOT_SUPPORTED.
    match result {
        Ok(()) => Ok(()),
        Err(zx::Status::NOT_SUPPORTED) => Ok(()),
        Err(_) => Err(anyhow!("wrong status returned: {:?}", result)),
    }
}
