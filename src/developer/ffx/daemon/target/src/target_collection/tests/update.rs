// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::*;
use assert_matches::assert_matches;
use fidl_fuchsia_developer_remotecontrol::RemoteControlMarker;
use fidl_fuchsia_overnet_protocol::NodeId;
use rcs::RcsConnection;

const NODENAME: &str = "nodename";
const SERIAL: &str = "serial";
const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 2324);

#[fuchsia_async::run_singlethreaded(test)]
async fn test_update_single() {
    let tc = TargetCollection::new_with_queue();

    let t = tc.merge_insert(Target::new());

    tc.update_target(
        &[TargetUpdateFilter::Ids(&[t.id()])],
        TargetUpdateBuilder::new().identity(Identity::from_serial(SERIAL)).build(),
        true,
    );

    assert_eq!(t.serial().as_deref(), Some(SERIAL));

    tc.update_target(
        &[TargetUpdateFilter::Serial(SERIAL)],
        TargetUpdateBuilder::new().identity(Identity::from_name(NODENAME)).build(),
        true,
    );

    assert_eq!(t.nodename().as_deref(), Some(NODENAME));

    tc.update_target(
        &[TargetUpdateFilter::LegacyNodeName(NODENAME)],
        TargetUpdateBuilder::new()
            .discovered(TargetProtocol::Fastboot, TargetTransport::Network)
            .net_addresses(&[ADDR])
            .build(),
        true,
    );

    assert_eq!(&Vec::from_iter(t.addrs().into_iter().map(|addr| SocketAddr::from(addr))), &[ADDR]);
    assert_matches!(t.get_connection_state(), TargetConnectionState::Fastboot(_));

    let local_node = overnet_core::Router::new(None).unwrap();

    let (proxy, _stream) = fidl::endpoints::create_proxy_and_stream::<RemoteControlMarker>();

    let conn = RcsConnection::new_with_proxy(local_node, proxy, &NodeId { id: 1234 });

    tc.update_target(
        &[TargetUpdateFilter::NetAddrs(&[ADDR])],
        TargetUpdateBuilder::new().enable().rcs(conn).build(),
        true,
    );

    assert!(t.rcs().is_some());

    tc.update_target(
        &[TargetUpdateFilter::OvernetNodeId(1234)],
        TargetUpdateBuilder::new().disconnected().build(),
        true,
    );

    assert!(t.rcs().is_none());
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_update_create_new() {
    let tc = TargetCollection::new_with_queue();

    tc.update_target(
        &[TargetUpdateFilter::Ids(&[1234])],
        TargetUpdateBuilder::new()
            .identity(Identity::from_serial(SERIAL))
            .enable()
            .discovered(TargetProtocol::Fastboot, TargetTransport::Network)
            .build(),
        true,
    );

    let targets = Vec::from_iter(tc.targets.borrow().values().cloned());
    let [t] = &targets[..] else { panic!("too many targets: {targets:?}") };

    assert_eq!(t.serial().as_deref(), Some(SERIAL));
    assert!(t.is_enabled());
    assert_matches!(t.get_connection_state(), TargetConnectionState::Fastboot(_));
}

#[fuchsia_async::run_singlethreaded(test)]
async fn test_update_multi() {
    let tc = TargetCollection::new_with_queue();

    let t = tc.merge_insert(Target::new());
    let t2 = tc.merge_insert(Target::new());

    tc.update_target(
        &[TargetUpdateFilter::Ids(&[t.id()])],
        TargetUpdateBuilder::new().identity(Identity::from_serial(SERIAL)).build(),
        true,
    );

    tc.update_target(
        &[TargetUpdateFilter::Ids(&[t2.id()])],
        TargetUpdateBuilder::new().identity(Identity::from_name(NODENAME)).build(),
        true,
    );

    // Sharing IDs in an update doesn't mean other pieces of state will be merged. The identity of
    // the updated targets will still be separate.
    tc.update_target(
        &[TargetUpdateFilter::Ids(&[t.id(), t2.id()])],
        TargetUpdateBuilder::new().enable().build(),
        true,
    );

    assert_eq!(t.serial().as_deref(), Some(SERIAL));
    assert_eq!(t.nodename().as_deref(), None);
    assert!(t.is_enabled());

    assert_eq!(t2.serial().as_deref(), None);
    assert_eq!(t2.nodename().as_deref(), Some(NODENAME));
    assert!(t2.is_enabled());
}
