// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::prelude::*,
    fidl_fuchsia_driver_test as fdt, fidl_fuchsia_io as fio,
    fidl_test_structuredconfig_receiver as scr, fidl_test_structuredconfig_receiver_shim as scrs,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{Capability, RealmBuilder, Ref, Route},
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    futures::StreamExt,
    std::time::Duration,
    tracing::info,
};

enum IncomingRequest {
    Puppet(scr::ConfigReceiverPuppetRequestStream),
}

async fn connect_to_config_service(
    expose_dir: &fio::DirectoryProxy,
) -> anyhow::Result<scrs::ConfigServiceProxy> {
    // Find an instance of `ConfigService`.
    let instance_name;
    let service =
        fuchsia_component::client::open_service_at_dir::<scrs::ConfigServiceMarker>(expose_dir)?;
    loop {
        // TODO(fxbug.dev/4776): Once component manager supports watching for
        // service instances, this loop should be replaced by a watcher.
        let entries = fuchsia_fs::directory::readdir(&service).await?;
        if let Some(entry) = entries.iter().next() {
            instance_name = entry.name.clone();
            break;
        }
        fasync::Timer::new(Duration::from_millis(100)).await;
    }

    // Connect to `ConfigService`.
    fuchsia_component::client::connect_to_service_instance_at_dir::<scrs::ConfigServiceMarker>(
        expose_dir,
        &instance_name,
    )
}

#[fuchsia::main]
async fn main() -> anyhow::Result<()> {
    // Create the RealmBuilder and start the driver.
    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_manifest_setup("#meta/realm.cm").await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::service::<scrs::ConfigServiceMarker>())
                .from(Ref::child(fuchsia_driver_test::COMPONENT_NAME))
                .to(Ref::parent()),
        )
        .await?;
    let realm = builder.build().await?;

    let exposes = vec![fdt::Expose {
        service_name: "test.structuredconfig.receiver.shim.ConfigService".to_string(),
        collection: fdt::Collection::PackageDrivers,
    }];

    let args = fdt::RealmArgs {
        root_driver: Some("#meta/cpp_driver_receiver.cm".to_string()),
        use_driver_framework_v2: Some(true),
        exposes: Some(exposes),
        ..fdt::RealmArgs::EMPTY
    };
    info!("about to start driver test realm");
    realm.driver_test_realm_start(args).await?;
    info!("started driver test realm");

    let config_service = connect_to_config_service(realm.root.get_exposed_dir()).await?;

    // Serve this configuration back to the test
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(IncomingRequest::Puppet);
    fs.take_and_serve_directory_handle().unwrap();
    fs.for_each_concurrent(None, |request: IncomingRequest| async {
        match request {
            IncomingRequest::Puppet(stream) => {
                // TOOD(fxbug.dev/121847): Make this conversion less verbose.
                let server_end: fidl::endpoints::ServerEnd<scr::ConfigReceiverPuppetMarker> =
                    std::sync::Arc::try_unwrap(stream.into_inner().0)
                        .unwrap()
                        .into_channel()
                        .into_zx_channel()
                        .into();
                config_service.connect_channel_to_puppet(server_end).unwrap()
            }
        }
    })
    .await;
    Ok(())
}
