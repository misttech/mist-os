// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error, Result},
    fidl::endpoints::DiscoverableProtocolMarker,
    fidl_fuchsia_boot as fboot, fidl_fuchsia_driver_development as fdd,
    fidl_fuchsia_driver_test as fdt, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_component_test::{
        Capability, ChildOptions, ChildRef, LocalComponentHandles, RealmBuilder, Route,
    },
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    fuchsia_zircon as zx,
    futures::{FutureExt as _, StreamExt as _},
    vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope, path::Path, service},
};

async fn get_driver_info(
    service: &fdd::DriverDevelopmentProxy,
    driver_filter: &[String],
) -> Result<Vec<fdd::DriverInfo>> {
    let (iterator, iterator_server) =
        fidl::endpoints::create_proxy::<fdd::DriverInfoIteratorMarker>()?;

    service
        .get_driver_info(driver_filter, iterator_server)
        .context("FIDL call to get driver info failed")?;

    let mut info_result = Vec::new();
    loop {
        let mut driver_info =
            iterator.get_next().await.context("FIDL call to get driver info failed")?;
        if driver_info.len() == 0 {
            break;
        }
        info_result.append(&mut driver_info)
    }
    Ok(info_result)
}

async fn serve_boot_items(handles: LocalComponentHandles) -> Result<(), Error> {
    let export = vfs::pseudo_directory! {
        "svc" => vfs::pseudo_directory! {
            fboot::ItemsMarker::PROTOCOL_NAME => service::host(move |stream| {
                run_boot_items(stream)
            }),
        },
    };

    let scope = ExecutionScope::new();
    export.open(
        scope.clone(),
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
        Path::dot(),
        fidl::endpoints::ServerEnd::from(handles.outgoing_dir.into_channel()),
    );
    scope.wait().await;

    Ok(())
}

async fn run_boot_items(mut stream: fboot::ItemsRequestStream) {
    /// This constant is defined in
    /// sdk/lib/zbi-format/include/lib/zbi-format/zbi.h.
    const ZBI_TYPE_PLATFORM_ID: u32 = 0x44494c50;

    /// These constants are defined in
    /// zircon/system/ulib/ddk-platform-defs/include/lib/ddk/platform-defs.h
    const PDEV_VID_TEST: u32 = 0x11;
    const PDEV_PID_PBUS_TEST: u32 = 0x01;

    /// This struct is defined in sdk/lib/zbi-format/include/lib/zbi-format/board.h
    struct ZbiPlatformId {
        _vid: u32,
        _pid: u32,
        _board_name: [u8; 32],
    }

    while let Some(request) = stream.next().await {
        match request.unwrap() {
            fboot::ItemsRequest::Get { type_, extra: _, responder } => {
                if type_ == ZBI_TYPE_PLATFORM_ID {
                    let platform_id = ZbiPlatformId {
                        _vid: PDEV_VID_TEST,
                        _pid: PDEV_PID_PBUS_TEST,
                        _board_name: [0; 32],
                    };
                    const PLATFORM_ID_SIZE: usize = std::mem::size_of::<ZbiPlatformId>();
                    let vmo = zx::Vmo::create(PLATFORM_ID_SIZE as u64).unwrap();
                    let bytes = unsafe {
                        std::mem::transmute::<ZbiPlatformId, [u8; PLATFORM_ID_SIZE]>(platform_id)
                    };
                    vmo.write(&bytes, PLATFORM_ID_SIZE as u64).unwrap();
                    responder.send(Some(vmo), PLATFORM_ID_SIZE as u32).unwrap();
                } else {
                    responder.send(None, 0).unwrap();
                }
            }
            fboot::ItemsRequest::Get2 { responder, type_: _, extra: _ } => {
                responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw())).unwrap();
            }
            fboot::ItemsRequest::GetBootloaderFile { responder, filename: _ } => {
                responder.send(None).unwrap();
            }
        }
    }
}

#[fasync::run_singlethreaded(test)]
async fn test_smoke_test() -> Result<()> {
    let realm = RealmBuilder::new().await?;
    realm.driver_test_realm_setup().await?;

    let instance = realm.build().await?;

    instance.driver_test_realm_start(fdt::RealmArgs::default()).await?;

    // Connect to a protocol to ensure that it starts, then immediately exit.
    let _ = instance.root.connect_to_protocol_at_exposed_dir::<fdd::DriverDevelopmentMarker>()?;
    Ok(())
}

// Run DriverTestRealm with no arguments and see that the drivers in our package
// are loaded.
#[fasync::run_singlethreaded(test)]
async fn test_empty_args() -> Result<()> {
    let realm = RealmBuilder::new().await?;
    realm.driver_test_realm_setup().await?;

    let instance = realm.build().await?;

    instance.driver_test_realm_start(fdt::RealmArgs::default()).await?;

    let driver_dev =
        instance.root.connect_to_protocol_at_exposed_dir::<fdd::DriverDevelopmentMarker>()?;

    let info = get_driver_info(&driver_dev, &[]).await?;
    assert!(info
        .iter()
        .any(|d| d.url == Some("fuchsia-boot:///#meta/test-parent-sys.cm".to_string())));
    assert!(info.iter().any(|d| d.url == Some("fuchsia-boot:///#meta/test.cm".to_string())));

    Ok(())
}

// Manually open our /pkg directory and pass it to DriverTestRealm to see that it works.
#[fasync::run_singlethreaded(test)]
async fn test_pkg_dir() -> Result<()> {
    let realm = RealmBuilder::new().await?;
    realm.driver_test_realm_setup().await?;

    let instance = realm.build().await?;

    let (pkg, pkg_server) = fidl::endpoints::create_endpoints::<fio::DirectoryMarker>();
    let pkg_flags = fuchsia_fs::OpenFlags::RIGHT_READABLE
        | fuchsia_fs::OpenFlags::RIGHT_EXECUTABLE
        | fio::OpenFlags::DIRECTORY;
    fuchsia_fs::directory::open_channel_in_namespace("/pkg", pkg_flags, pkg_server).unwrap();
    let args = fdt::RealmArgs { boot: Some(pkg), ..Default::default() };

    instance.driver_test_realm_start(args).await?;

    let driver_dev =
        instance.root.connect_to_protocol_at_exposed_dir::<fdd::DriverDevelopmentMarker>()?;

    let info = get_driver_info(&driver_dev, &[]).await?;
    assert!(info
        .iter()
        .any(|d| d.url == Some("fuchsia-boot:///#meta/test-parent-sys.cm".to_string())));
    assert!(info.iter().any(|d| d.url == Some("fuchsia-boot:///#meta/test.cm".to_string())));

    let dev = instance.driver_test_realm_connect_to_dev()?;
    device_watcher::recursive_wait(&dev, "sys/test/test").await?;

    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_root_driver() -> Result<()> {
    let realm = RealmBuilder::new().await?;
    realm.driver_test_realm_setup().await?;

    let instance = realm.build().await?;
    let args = fdt::RealmArgs {
        root_driver: Some("fuchsia-boot:///#meta/platform-bus.cm".to_string()),
        ..Default::default()
    };

    instance.driver_test_realm_start(args).await?;

    let dev = instance.driver_test_realm_connect_to_dev()?;
    device_watcher::recursive_wait(&dev, "sys/platform").await?;

    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_tunnel_boot_items() -> Result<()> {
    let realm = RealmBuilder::new().await?;
    realm.driver_test_realm_setup().await?;

    let boot_items = realm
        .add_local_child("boot_items", move |h| serve_boot_items(h).boxed(), ChildOptions::new())
        .await?;
    let driver_test_realm: ChildRef = fuchsia_driver_test::COMPONENT_NAME.into();
    realm
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fboot::ItemsMarker>())
                .from(&boot_items)
                .to(&driver_test_realm),
        )
        .await?;

    realm.init_mutable_config_from_package(&driver_test_realm).await?;
    realm.set_config_value_bool(&driver_test_realm, "tunnel_boot_items", true).await?;

    let instance = realm.build().await?;
    let args = fdt::RealmArgs {
        root_driver: Some("fuchsia-boot:///#meta/platform-bus.cm".to_string()),
        ..Default::default()
    };

    instance.driver_test_realm_start(args).await?;

    let dev = instance.driver_test_realm_connect_to_dev()?;
    device_watcher::recursive_wait(&dev, "sys/platform").await?;

    Ok(())
}
