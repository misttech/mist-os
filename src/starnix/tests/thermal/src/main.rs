// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use common::EXPECTED_TEMP_C;
use component_events::events::{EventStream, ExitStatus, Stopped};
use component_events::matcher::EventMatcher;
use fuchsia_component::client::Service;
use fuchsia_component_test::{Capability, RealmBuilder, RealmBuilderParams, Ref, Route};
use fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance};
use log::info;
use {fidl_fuchsia_driver_test as fdt, fidl_fuchsia_io as fio};

#[fuchsia::main]
async fn main() {
    let mut events = EventStream::open().await.unwrap();
    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new()
            .realm_name("thermal_test")
            .from_relative_url("#meta/container_with_thermal_client.cm"),
    )
    .await
    .unwrap();
    builder.driver_test_realm_setup().await.unwrap();

    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::directory("dev-class")
                        .as_("dev-trippoint")
                        .subdir("trippoint")
                        .rights(fio::R_STAR_DIR),
                )
                .from(Ref::child(fuchsia_driver_test::COMPONENT_NAME))
                .to(Ref::child("kernel")),
        )
        .await
        .unwrap();

    let dtr_exposes = vec![Capability::service::<fidl_test_trippoint::ServiceMarker>().into()];
    builder.driver_test_realm_add_dtr_exposes(&dtr_exposes).await.unwrap();

    info!("starting realm");
    let instance = builder.build().await.unwrap();
    instance
        .driver_test_realm_start(fdt::RealmArgs {
            dtr_exposes: Some(dtr_exposes),
            ..Default::default()
        })
        .await
        .unwrap();

    let control =
        Service::open_from_dir(instance.root.get_exposed_dir(), fidl_test_trippoint::ServiceMarker)
            .unwrap()
            .watch_for_any()
            .await
            .unwrap()
            .connect_to_control()
            .unwrap();
    control.set_temperature_celsius(zx::Status::OK.into_raw(), EXPECTED_TEMP_C).await.unwrap();

    let realm_moniker = format!("realm_builder:{}", instance.root.child_name());
    info!(realm_moniker:%; "started");
    let thermal_client_moniker = format!("{realm_moniker}/thermal_client");

    info!(thermal_client_moniker:%; "waiting for thermal_client to exit");
    let stopped = EventMatcher::ok()
        .moniker(&thermal_client_moniker)
        .wait::<Stopped>(&mut events)
        .await
        .unwrap();
    let status = stopped.result().unwrap().status;
    info!(status:?; "thermal_client stopped");
    assert_eq!(status, ExitStatus::Clean);
}
