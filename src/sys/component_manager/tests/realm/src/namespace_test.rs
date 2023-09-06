// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::ServerEnd;
use fidl_fidl_examples_routing_echo::EchoMarker;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_sys2 as fsys;
use fuchsia_component::client::*;

#[fuchsia::test]
pub async fn connect_to_incoming_capabilities_for_component_without_program() {
    let query = connect_to_protocol::<fsys::RealmQueryMarker>().unwrap();

    // We can't open the protocol in the component namespace until the component is resolved.
    let (_echo, server_end) = fidl::endpoints::create_proxy::<EchoMarker>().unwrap();
    assert!(query
        .open(
            "no_program",
            fsys::OpenDirType::NamespaceDir,
            fio::OpenFlags::empty(),
            fio::ModeType::empty(),
            "/svc/fidl.examples.routing.echo.Echo",
            ServerEnd::new(server_end.into_channel()),
        )
        .await
        .unwrap()
        .is_err());

    // Trigger resolution.
    let lifecycle =
        connect_to_protocol::<fsys::LifecycleControllerMarker>().expect("connected to lifecycle");
    let _ = lifecycle
        .resolve_instance("./no_program")
        .await
        .expect("fidl ok")
        .expect("resolved instance");

    // Open protocol in the component namespace.
    let (echo, server_end) = fidl::endpoints::create_proxy::<EchoMarker>().unwrap();
    query
        .open(
            "no_program",
            fsys::OpenDirType::NamespaceDir,
            fio::OpenFlags::empty(),
            fio::ModeType::empty(),
            "/svc/fidl.examples.routing.echo.Echo",
            ServerEnd::new(server_end.into_channel()),
        )
        .await
        .unwrap()
        .unwrap();
    let reply = echo.echo_string(Some("test")).await.expect("called echo string");
    assert_eq!(reply.unwrap(), "test");
}
