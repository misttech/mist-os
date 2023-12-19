// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    fidl::endpoints::ControlHandle,
    fidl::endpoints::ServerEnd,
    fidl_test_wlan_realm as fidl_realm,
    fidl_test_wlan_realm::*,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{
        Capability, ChildOptions, RealmBuilder, RealmBuilderParams, RealmInstance, Ref, Route,
    },
    fuchsia_driver_test::DriverTestRealmBuilder,
    fuchsia_zircon_status as zx_status,
    futures::{StreamExt, TryStreamExt},
    tracing::info,
};

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(|stream: RealmFactoryRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_realm_factory).await;
    Ok(())
}

async fn serve_realm_factory(mut stream: RealmFactoryRequestStream) {
    let mut task_group = fasync::TaskGroup::new();
    let result: Result<(), Error> = async move {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                RealmFactoryRequest::_UnknownMethod { control_handle, .. } => {
                    control_handle.shutdown_with_epitaph(zx_status::Status::NOT_SUPPORTED);
                    unimplemented!();
                }
                RealmFactoryRequest::CreateRealm { options, realm_server, responder } => {
                    let realm = create_realm(options).await?;
                    let request_stream = realm_server.into_stream()?;
                    task_group.spawn(async move {
                        realm_proxy::service::serve(realm, request_stream).await.unwrap();
                    });
                    responder.send(Ok(()))?;
                }
            }
        }

        task_group.join().await;
        Ok(())
    }
    .await;

    if let Err(err) = result {
        // hw-sim tests allow error logs so we panic to ensure test failure.
        panic!("{:?}", err);
    }
}

async fn create_realm(mut options: fidl_realm::RealmOptions) -> Result<RealmInstance, Error> {
    info!("building the realm using options {:#?}", options);

    let wlan_config =
        options.wlan_config.unwrap_or(fidl_realm::WlanConfig { ..Default::default() });

    let mut params = RealmBuilderParams::new();
    if let Some(ref name) = wlan_config.name {
        params = params.realm_name(name);
    }
    let builder = RealmBuilder::with_params(params).await?;

    builder.driver_test_realm_setup().await?;
    create_wlan_components(&builder, wlan_config).await?;
    let realm = builder.build().await?;

    let devfs = options.devfs_server_end.take().unwrap();
    realm.root.get_exposed_dir().open(
        fidl_fuchsia_io::OpenFlags::DIRECTORY,
        fidl_fuchsia_io::ModeType::empty(),
        "dev-topological",
        ServerEnd::new(devfs.into_channel()),
    )?;

    Ok(realm)
}

async fn create_wlan_components(
    builder: &RealmBuilder,
    config: fidl_realm::WlanConfig,
) -> Result<(), Error> {
    // Create child components.
    let archivist = builder
        .add_child("archivist", "#meta/archivist-for-embedding.cm", ChildOptions::new())
        .await?;

    let wlandevicemonitor = builder
        .add_child("wlandevicemonitor", "#meta/wlandevicemonitor.cm", ChildOptions::new())
        .await?;

    let wlancfg = builder.add_child("wlancfg", "#meta/wlancfg.cm", ChildOptions::new()).await?;

    let stash = builder.add_child("stash", "#meta/stash_secure.cm", ChildOptions::new()).await?;

    // Configure components
    let use_legacy_privacy = config.use_legacy_privacy.unwrap_or(false);
    builder.init_mutable_config_to_empty(&wlandevicemonitor).await?;
    builder.set_config_value_bool(&wlandevicemonitor, "wep_supported", use_legacy_privacy).await?;
    builder.set_config_value_bool(&wlandevicemonitor, "wpa1_supported", use_legacy_privacy).await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.wlan.policy.ClientProvider"))
                .capability(Capability::protocol_by_name("fuchsia.wlan.policy.AccessPointProvider"))
                .from(&wlancfg)
                .to(Ref::parent()),
        )
        .await?;

    // fuchsia.wlan.device.service.DeviceMonitor is used by set_country
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name(
                    "fuchsia.wlan.device.service.DeviceMonitor",
                ))
                .from(&wlandevicemonitor)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.diagnostics.ArchiveAccessor"))
                .capability(Capability::protocol_by_name("fuchsia.inspect.InspectSink"))
                .from(&archivist)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .from(Ref::parent())
                .to(&archivist)
                .to(&wlancfg)
                .to(&wlandevicemonitor)
                .to(&stash),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::event_stream("directory_ready"))
                .capability(Capability::event_stream("capability_requested"))
                .from(Ref::parent())
                .to(&archivist),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::storage("data"))
                .from(Ref::parent())
                .to(&stash)
                .to(&wlancfg),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.inspect.InspectSink"))
                .from(&archivist)
                .to(&wlancfg)
                .to(&wlandevicemonitor),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol_by_name("fuchsia.wlan.device.service.DeviceMonitor")
                        .weak(),
                )
                .from(&wlandevicemonitor)
                .to(&wlancfg),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::directory("dev-class").subdir("wlanphy").as_("dev-wlanphy"))
                .from(Ref::child(fuchsia_driver_test::COMPONENT_NAME))
                .to(&wlandevicemonitor),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.stash.SecureStore"))
                .from(&stash)
                .to(&wlancfg),
        )
        .await?;

    // Handle optional components based on config
    if config.with_regulatory_region.unwrap_or(true) {
        let regulatory_region = builder
            .add_child("regulatory_region", "#meta/regulatory_region.cm", ChildOptions::new())
            .await?;

        builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::protocol_by_name(
                            "fuchsia.location.namedplace.RegulatoryRegionWatcher",
                        )
                        .weak(),
                    )
                    .from(&regulatory_region)
                    .to(&wlancfg),
            )
            .await?;

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                    .capability(Capability::storage("cache"))
                    .from(Ref::parent())
                    .to(&regulatory_region),
            )
            .await?;
    }

    Ok(())
}
