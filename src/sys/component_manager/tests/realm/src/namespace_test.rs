// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fidl_examples_routing_echo::EchoMarker;
use fidl_fuchsia_sys2 as fsys;
use fuchsia_component::client::*;

#[fuchsia::test]
pub async fn connect_to_incoming_capabilities_for_component_without_program() {
    let query = connect_to_protocol::<fsys::RealmQueryMarker>().unwrap();

    // Trigger resolution.
    let lifecycle =
        connect_to_protocol::<fsys::LifecycleControllerMarker>().expect("connected to lifecycle");
    let _ = lifecycle
        .resolve_instance("./no_program")
        .await
        .expect("fidl ok")
        .expect("resolved instance");

    // Create incoming namespace.
    let namespace = query.construct_namespace("./no_program").await.unwrap().unwrap();
    let svc_entry = namespace
        .into_iter()
        .find(|entry| matches!(&entry.path, Some(path) if path == "/svc"))
        .expect("found Echo entry");
    let svc_dir = svc_entry
        .directory
        .expect("svc has a directory handle")
        .into_proxy()
        .expect("get DirectoryProxy from handle");
    let echo = connect_to_protocol_at_dir_root::<EchoMarker>(&svc_dir).expect("connect to Echo");
    let reply = echo.echo_string(Some("test")).await.expect("called echo string");
    assert_eq!(reply.unwrap(), "test");
}
