// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use component_events::events::{EventStream, ExitStatus, Stopped};
use component_events::matcher::EventMatcher;
use fake_socket_tunnel::mock_socket_tunnel;
use fidl_fuchsia_hardware_sockettunnel::DeviceMarker;
use fuchsia_component_test::{
    Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmBuilderParams, Ref, Route,
};
use log::info;
mod fake_socket_tunnel;

#[fuchsia::main]
async fn main() {
    let mut events = EventStream::open().await.unwrap();
    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new()
            .realm_name("nanohub_test")
            .from_relative_url("#meta/container_with_sysfs_reader.cm"),
    )
    .await
    .unwrap();

    let socket_tunnel_mock = builder
        .add_local_child(
            "fake_socket_tunnel",
            move |handles: LocalComponentHandles| Box::pin(mock_socket_tunnel(handles)),
            ChildOptions::new(),
        )
        .await
        .unwrap();

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<DeviceMarker>())
                .from(&socket_tunnel_mock)
                .to(Ref::child("kernel")),
        )
        .await
        .unwrap();

    info!("starting realm");
    let kernel_with_container_and_sysfs_reader = builder.build().await.unwrap();
    let realm_moniker =
        format!("realm_builder:{}", kernel_with_container_and_sysfs_reader.root.child_name());
    info!(realm_moniker:%; "started");
    let sysfs_reader_moniker = format!("{realm_moniker}/nanohub_user");

    // Wait for the sysfs_reader program to run...
    info!(sysfs_reader_moniker:%; "waiting for sysfs_reader to exit");
    let stopped = EventMatcher::ok()
        .moniker(&sysfs_reader_moniker)
        .wait::<Stopped>(&mut events)
        .await
        .unwrap();
    let status = stopped.result().unwrap().status;
    info!(status:?; "sysfs_reader stopped");
    assert_eq!(status, ExitStatus::Clean);
}
