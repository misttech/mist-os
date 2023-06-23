// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fidl_examples_routing_echo as fecho, fuchsia_component_test::ScopedInstance, tracing::info,
};

#[fuchsia::test]
async fn core_proxy() {
    let core =
        ScopedInstance::new("realm_builder".into(), "#meta/fake_core.cm".into()).await.unwrap();
    info!("binding to Echo");
    let exposed = core.get_exposed_dir();
    let svc_for_sys = fuchsia_fs::directory::open_directory(
        exposed,
        "svc_for_sys",
        fuchsia_fs::OpenFlags::empty(),
    )
    .await
    .unwrap();
    let echo = fuchsia_component::client::connect_to_protocol_at_dir_root::<fecho::EchoMarker>(
        &svc_for_sys,
    )
    .unwrap();
    let out = echo.echo_string(Some("world")).await.unwrap();
    info!("received echo response");
    assert_eq!(out.unwrap(), "world");
}
