// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// This module tests the property that the FuchsiaBootResolver successfully
/// resolves components that are encoded in a meta.far. This test is fully
/// hermetic.
use {
    fidl::endpoints,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_io as fio, fidl_fuchsia_process as fprocess,
    fidl_fuchsia_sys2 as fsys,
    fuchsia_component_test::ScopedInstance,
    fuchsia_runtime::{HandleInfo, HandleType},
    std::{fs::File, io::Read},
    zx::{self as zx, HandleBased},
};

// macros
use vfs::assert_read_dirents;

use vfs::directory::test_utils::DirentsSameInodeBuilder;

const ZBI_PATH: &str = "/pkg/data/tests/uncompressed_bootfs";

// Create a vmo of the test bootfs image that can be decommitted by BootfsSvc.
#[cfg(test)]
fn read_file_to_vmo(path: &str) -> zx::Vmo {
    let mut file_buffer = Vec::new();
    File::open(path).and_then(|mut f| f.read_to_end(&mut file_buffer)).unwrap();
    let vmo = zx::Vmo::create(file_buffer.len() as u64).unwrap();
    vmo.write(&file_buffer, 0).unwrap();
    vmo
}

#[fuchsia::test]
async fn package_resolution() {
    let vmo = read_file_to_vmo(ZBI_PATH);
    let numbered_handles = vec![fprocess::HandleInfo {
        handle: vmo.into_handle(),
        id: HandleInfo::from(HandleType::BootfsVmo).as_raw(),
    }];
    let instance =
        ScopedInstance::new("coll".into(), "#meta/component_manager.cm".into()).await.unwrap();
    let args = fcomponent::StartChildArgs {
        numbered_handles: Some(numbered_handles),
        ..Default::default()
    };
    let _cm_controller = instance.start_with_args(args).await.unwrap();

    // Confirm root component (trigger.cm) can be started.
    let lifecycle_controller =
        instance.connect_to_protocol_at_exposed_dir::<fsys::LifecycleControllerMarker>().unwrap();
    let (_binder, server_end) = endpoints::create_proxy::<fcomponent::BinderMarker>().unwrap();
    // Confirm root component (hello_world.cm) can be started.
    lifecycle_controller.start_instance(".".into(), server_end).await.unwrap().unwrap();

    // Verify the contents of hello_world's /pkg match what we expect.
    let realm_query =
        instance.connect_to_protocol_at_exposed_dir::<fsys::RealmQueryMarker>().unwrap();
    let (dir_proxy, server_end) = endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    let server_end = endpoints::ServerEnd::new(server_end.into_channel());
    realm_query
        .open(
            ".".into(),
            fsys::OpenDirType::PackageDir,
            fio::OpenFlags::RIGHT_READABLE,
            fio::ModeType::empty(),
            ".",
            server_end,
        )
        .await
        .unwrap()
        .unwrap();

    let mut expected = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    expected
        .add(fio::DirentType::Directory, b".")
        .add(fio::DirentType::Directory, b"bin")
        .add(fio::DirentType::Directory, b"lib")
        .add(fio::DirentType::Directory, b"meta");

    assert_read_dirents!(dir_proxy, 1000, expected.into_vec());

    let mut expected_bin = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    expected_bin.add(fio::DirentType::Directory, b".").add(fio::DirentType::File, b"trigger");
    assert_read_dirents!(
        fuchsia_fs::directory::open_directory_no_describe_deprecated(
            &dir_proxy,
            "bin",
            fio::OpenFlags::empty(),
        )
        .unwrap(),
        1000,
        expected_bin.into_vec()
    );

    let mut expected_meta = DirentsSameInodeBuilder::new(fio::INO_UNKNOWN);
    expected_meta
        .add(fio::DirentType::Directory, b".")
        .add(fio::DirentType::File, b"contents")
        .add(fio::DirentType::Directory, b"fuchsia.abi")
        .add(fio::DirentType::File, b"package")
        .add(fio::DirentType::File, b"trigger.cm");

    assert_read_dirents!(
        fuchsia_fs::directory::open_directory_no_describe_deprecated(
            &dir_proxy,
            "meta",
            fio::OpenFlags::empty(),
        )
        .unwrap(),
        1000,
        expected_meta.into_vec()
    );
}
