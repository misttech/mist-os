// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl_fuchsia_bluetooth_avrcp::{PeerManagerMarker, PeerManagerProxy};
use fidl_fuchsia_bluetooth_avrcp_test::{PeerManagerExtMarker, PeerManagerExtProxy};
use fidl_fuchsia_bluetooth_bredr::{ProfileMarker, ProfileRequest};
use fuchsia_component_test::{
    Capability, ChildOptions, LocalComponentHandles, RealmBuilder, Ref, Route,
};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use log::info;
use realmbuilder_mock_helpers::mock_component;

/// AVRCP component URL.
const AVRCP_URL: &str = "fuchsia-pkg://fuchsia.com/bt-avrcp-smoke-test#meta/bt-avrcp.cm";

/// The different events generated by this test.
/// Note: In order to prevent the component under test from terminating, any FIDL request or
/// Proxy is preserved.
enum Event {
    /// A BR/EDR Profile event.
    Profile { _profile: Option<ProfileRequest> },
    /// AVRCP PeerManager event.
    PeerManager { _peer_manager: Option<PeerManagerProxy> },
    /// AVRCP PeerManagerExt event.
    PeerManagerExt { _peer_manager_ext: Option<PeerManagerExtProxy> },
}

impl From<ProfileRequest> for Event {
    fn from(src: ProfileRequest) -> Self {
        Self::Profile { _profile: Some(src) }
    }
}

/// Represents a fake AVRCP client that requests the PeerManager and PeerManagerExt services.
async fn mock_avrcp_client(
    mut sender: mpsc::Sender<Event>,
    handles: LocalComponentHandles,
) -> Result<(), Error> {
    let peer_manager_svc: PeerManagerProxy = handles.connect_to_protocol()?;
    sender
        .send(Event::PeerManager { _peer_manager: Some(peer_manager_svc) })
        .await
        .expect("failed sending ack to test");

    let peer_manager_ext_svc: PeerManagerExtProxy = handles.connect_to_protocol()?;
    sender
        .send(Event::PeerManagerExt { _peer_manager_ext: Some(peer_manager_ext_svc) })
        .await
        .expect("failed sending ack to test");
    Ok(())
}

/// Tests that the v2 AVRCP component has the correct topology and verifies that
/// it connects and provides the expected services.
#[fuchsia::test]
async fn avrcp_v2_component_topology() {
    info!("Starting AVRCP v2 smoke test...");

    let (sender, mut receiver) = mpsc::channel(2);
    let profile_tx = sender.clone();
    let fake_client_tx = sender.clone();

    let builder = RealmBuilder::new().await.expect("Failed to create test realm builder");
    // The v2 component under test.
    let avcrp = builder
        .add_child("avrcp", AVRCP_URL.to_string(), ChildOptions::new())
        .await
        .expect("Failed adding avrcp to topology");
    // Mock Profile component to receive bredr.Profile requests.
    let fake_profile = builder
        .add_local_child(
            "fake-profile",
            move |handles: LocalComponentHandles| {
                let sender = profile_tx.clone();
                Box::pin(mock_component::<ProfileMarker, _>(sender, handles))
            },
            ChildOptions::new(),
        )
        .await
        .expect("Failed adding profile mock to topology");
    // Mock AVRCP client that will request the PeerManager and PeerManagerExt services
    // which are provided by `bt-avrcp.cml`.
    let fake_avcrp_client = builder
        .add_local_child(
            "fake-avrcp-client",
            move |handles: LocalComponentHandles| {
                let sender = fake_client_tx.clone();
                Box::pin(mock_avrcp_client(sender, handles))
            },
            ChildOptions::new().eager(),
        )
        .await
        .expect("Failed adding avrcp client mock to topology");

    // Set up capabilities.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<PeerManagerMarker>())
                .capability(Capability::protocol::<PeerManagerExtMarker>())
                .from(&avcrp)
                .to(&fake_avcrp_client),
        )
        .await
        .expect("Failed adding route for PeerManager services");
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<ProfileMarker>())
                .from(&fake_profile)
                .to(&avcrp),
        )
        .await
        .expect("Failed adding route for Profile service");
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fidl_fuchsia_logger::LogSinkMarker>())
                .from(Ref::parent())
                .to(&avcrp)
                .to(&fake_profile)
                .to(&fake_avcrp_client),
        )
        .await
        .expect("Failed adding LogSink route to test components");
    let test_topology = builder.build().await.unwrap();

    // If the routing is correctly configured, we expect four events:
    //   1: `bt-avrcp` connecting to the Profile service.
    //     a. Making a request to Advertise.
    //     b. Making a request to Search.
    //   2. `fake-avrcp-client` connecting to the PeerManager service which is provided by `bt-avrcp`.
    //   3. `fake-avrcp-client` connecting to the PeerManagerExt service which is provided by `bt-avrcp`.
    let mut events = Vec::new();
    for i in 0..4 {
        let msg = format!("Unexpected error waiting for {:?} event", i);
        events.push(receiver.next().await.expect(&msg));
    }
    assert_eq!(events.len(), 4);
    let discriminants: Vec<_> = events.iter().map(std::mem::discriminant).collect();
    assert_eq!(
        discriminants
            .iter()
            .filter(|&&d| d == std::mem::discriminant(&Event::Profile { _profile: None }))
            .count(),
        2
    );
    assert_eq!(
        discriminants
            .iter()
            .filter(|&&d| d == std::mem::discriminant(&Event::PeerManager { _peer_manager: None }))
            .count(),
        1
    );
    assert_eq!(
        discriminants
            .iter()
            .filter(|&&d| d
                == std::mem::discriminant(&Event::PeerManagerExt { _peer_manager_ext: None }))
            .count(),
        1
    );

    test_topology.destroy().await.unwrap();

    info!("Finished AVRCP smoke test");
}
