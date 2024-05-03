// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    fidl::endpoints::{ClientEnd, Proxy},
    fidl_test_suspendcontrol as tsc,
    fidl_test_systemactivitygovernor::*,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{
        Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route, DEFAULT_COLLECTION_NAME,
    },
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    futures::{StreamExt, TryStreamExt},
    tracing::*,
};

const ACTIVITY_GOVERNOR_CHILD_NAME: &str = "system-activity-governor";

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(|stream: RealmFactoryRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_realm_factory).await;
    Ok(())
}

async fn serve_realm_factory(stream: RealmFactoryRequestStream) {
    if let Err(err) = handle_request_stream(stream).await {
        error!("{:?}", err);
    }
}

async fn handle_request_stream(mut stream: RealmFactoryRequestStream) -> Result<()> {
    let mut task_group = fasync::TaskGroup::new();
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            RealmFactoryRequest::CreateRealm { realm_server, responder } => {
                let (realm, control_client_end) = create_realm().await?;
                let moniker = format!(
                    "{}:{}/{}",
                    DEFAULT_COLLECTION_NAME,
                    realm.root.child_name(),
                    ACTIVITY_GOVERNOR_CHILD_NAME
                );

                let request_stream = realm_server.into_stream()?;
                task_group.spawn(async move {
                    realm_proxy::service::serve(realm, request_stream).await.unwrap();
                });
                responder.send(Ok((&moniker, control_client_end)))?;
            }

            RealmFactoryRequest::_UnknownMethod { .. } => unreachable!(),
        }
    }

    task_group.join().await;
    Ok(())
}

async fn create_realm() -> Result<(RealmInstance, ClientEnd<tsc::DeviceMarker>), Error> {
    info!("building the realm");

    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_setup().await?;

    let component_ref = builder
        .add_child(
            ACTIVITY_GOVERNOR_CHILD_NAME,
            "#meta/system-activity-governor.cm",
            ChildOptions::new(),
        )
        .await?;

    let power_broker_ref =
        builder.add_child("power-broker", "#meta/power-broker.cm", ChildOptions::new()).await?;

    // Expose capabilities from power-broker.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.broker.Topology"))
                .from(&power_broker_ref)
                .to(Ref::parent()),
        )
        .await?;

    // Expose capabilities from power-broker to system-activity-governor.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.broker.Topology"))
                .from(&power_broker_ref)
                .to(&component_ref),
        )
        .await?;

    // Expose capabilities from driver-test-realm to system-activity-governor.
    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::directory("dev-class").subdir("suspend").as_("dev-class-suspend"),
                )
                .from(Ref::child(fuchsia_driver_test::COMPONENT_NAME))
                .to(&component_ref),
        )
        .await?;

    // Expose capabilities from system-activity-governor.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.suspend.Stats"))
                .capability(Capability::protocol_by_name("fuchsia.power.system.ActivityGovernor"))
                .capability(Capability::service_by_name(
                    "fuchsia.power.broker.ElementInfoProviderService",
                ))
                .from(&component_ref)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;
    realm.driver_test_realm_start(Default::default()).await?;
    let dev = realm.driver_test_realm_connect_to_dev()?;

    let dir_proxy = device_watcher::recursive_wait_and_open_directory(&dev, "class/test").await?;
    let entry = device_watcher::wait_for_device_with(&dir_proxy, |info| {
        info!("{:?} has topological path {:?}", info.filename, info.topological_path);
        info.topological_path.ends_with("fake-suspend/control").then_some(info.filename.to_string())
    })
    .await?;

    let control_client_end = device_watcher::recursive_wait_and_open::<tsc::DeviceMarker>(
        &dev,
        &format!("class/test/{}", entry),
    )
    .await?
    .into_client_end()
    .unwrap();

    Ok((realm, control_client_end))
}
