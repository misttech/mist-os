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
    zx::HandleBased,
};

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

    // Start the root component
    let lifecycle_controller =
        instance.connect_to_protocol_at_exposed_dir::<fsys::LifecycleControllerMarker>().unwrap();
    let (_binder, server_end) = endpoints::create_proxy::<fcomponent::BinderMarker>();
    lifecycle_controller.start_instance(".".into(), server_end).await.unwrap().unwrap();

    // Connect to the root component's test.checker.Check capability.
    let realm_query =
        instance.connect_to_protocol_at_exposed_dir::<fsys::RealmQueryMarker>().unwrap();
    let (exposed_dir, server_end) = endpoints::create_proxy::<fio::DirectoryMarker>();
    realm_query
        .open(
            ".".into(),
            fsys::OpenDirType::ExposedDir,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
            fio::ModeType::empty(),
            ".",
            server_end.into_channel().into(),
        )
        .await
        .unwrap()
        .unwrap();
    let checker = fuchsia_component::client::connect_to_protocol_at_dir_root::<
        fidl_test_checker::CheckerMarker,
    >(&exposed_dir)
    .unwrap();

    // The root component responding to FIDL requests at all suggests that bootfs package resolution
    // as used by the component resolver is working correctly.
    // If the contents of the sentinel file, which is contained in a package that is resolved by the
    // root component using CM's exposed bootfs fuchsia.pkg.PackageResolver capability, are as
    // expected, that suggests that bootfs package resolution as exposed as a CM builtin capability
    // is working correctly.
    assert_eq!(checker.sentinel_file_contents().await.unwrap(), "unguessable contents\n");
}
