// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::endpoints::create_proxy,
    fidl_fuchsia_component::RealmMarker,
    fidl_fuchsia_component_decl::ChildRef,
    fidl_test_ping::{PingMarker, PingRequest, PingRequestStream},
    fuchsia_async as fasync,
    fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at_dir_root},
    fuchsia_component::server::ServiceFs,
    futures::prelude::*,
};

enum IncomingRequest {
    Ping(PingRequestStream),
}

#[fasync::run_singlethreaded]
async fn main() {
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(IncomingRequest::Ping);
    fs.take_and_serve_directory_handle().expect("failed to take startup handle");
    fs.for_each_concurrent(0, |IncomingRequest::Ping(mut stream)| async move {
        let realm = connect_to_protocol::<RealmMarker>()
            .expect("failed to connect to fuchsia.component.Realm");
        let (subpackaged_component_exposed_dir, server_end) =
            create_proxy().expect("failed to create proxy");
        realm
            .open_exposed_dir(
                &ChildRef { name: "base-subpackaged-component".into(), collection: None },
                server_end,
            )
            .await
            .expect("failed to call open_exposed_dir FIDL")
            .expect("failed to open exposed dir of child");
        let forward_ping =
            connect_to_protocol_at_dir_root::<PingMarker>(&subpackaged_component_exposed_dir)
                .expect("failed to connect to Ping protocol");
        while let Some(PingRequest::Ping { ping, responder }) =
            stream.try_next().await.expect("failed to read request")
        {
            let expect_ping_pong =
                forward_ping.ping(&ping).await.expect("Forwarded Ping FIDL call failed");
            assert_eq!(expect_ping_pong, format!("{ping} pong"));
            responder
                .send(&format!("forwarded {} and returned {}", ping, expect_ping_pong))
                .expect("failed to send pong");
        }
    })
    .await;
}
