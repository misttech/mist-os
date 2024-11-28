// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Error};
use fidl::endpoints::RequestStream;
use fidl_fuchsia_bluetooth_host::{
    BondingDelegateRequestStream, DiscoverySessionRequestStream, HostControlHandle, HostMarker,
    HostRequest, HostRequestStream,
};
use fidl_fuchsia_bluetooth_sys::HostInfo as FidlHostInfo;
use fuchsia_bluetooth::types::bonding_data::example;
use fuchsia_bluetooth::types::{Address, BondingData, HostId, HostInfo, Peer, PeerId};
use fuchsia_sync::RwLock;
use futures::future::Either;
use futures::stream::StreamExt;
use futures::{future, join, pin_mut, FutureExt};
use std::sync::Arc;
use test_util::assert_gt;

use crate::host_device::test::FakeHostServer;
use crate::host_device::{HostDevice, HostListener};

// An impl that ignores all events
impl HostListener for () {
    type PeerUpdatedFut = future::Ready<()>;
    fn on_peer_updated(&mut self, _peer: Peer) -> Self::PeerRemovedFut {
        future::ready(())
    }

    type PeerRemovedFut = future::Ready<()>;
    fn on_peer_removed(&mut self, _id: PeerId) -> Self::PeerRemovedFut {
        future::ready(())
    }

    type HostBondFut = future::Ready<Result<(), anyhow::Error>>;
    fn on_new_host_bond(&mut self, _data: BondingData) -> Self::HostBondFut {
        future::ok(())
    }

    type HostInfoFut = future::Ready<Result<(), anyhow::Error>>;
    fn on_host_updated(&mut self, _info: HostInfo) -> Self::HostInfoFut {
        future::ok(())
    }
}

// Create a HostDevice with a fake channel, set local name and check it is updated
#[fuchsia::test]
async fn host_device_set_local_name() -> Result<(), Error> {
    let (client, server) = fidl::endpoints::create_proxy_and_stream::<HostMarker>();
    let address = Address::Public([0, 0, 0, 0, 0, 0]);
    let host = HostDevice::mock(HostId(1), address, "/dev/class/bt-hci/test".to_string(), client);
    let expected_name = "EXPECTED_NAME".to_string();
    let info = Arc::new(RwLock::new(host.info()));
    let server = Arc::new(RwLock::new(server));
    let _delegate = expect_set_bonding_delegate(server.clone()).await?;

    // Assign a name and verify that that it gets written to the bt-host device over FIDL.
    let set_name = host.set_name(expected_name.clone());
    let expect_fidl = expect_call(server.clone(), |_, e| match e {
        HostRequest::SetLocalName { local_name, responder } => {
            info.write().local_name = Some(local_name);
            responder.send(Ok(()))?;
            Ok(())
        }
        _ => Err(format_err!("Unexpected!")),
    });
    let (set_name_result, expect_result) = join!(set_name, expect_fidl);
    let _ = set_name_result.expect("failed to set name");
    let _ = expect_result.expect("FIDL result unsatisfied");

    refresh_host(host.clone(), server.clone(), info.read().clone()).await;
    let host_name = host.info().local_name.clone();
    assert!(host_name == Some(expected_name));
    Ok(())
}

// Test that we can establish a host discovery session, then stop discovery on the host when
// the discovery proxy is dropped.
#[fuchsia::test]
async fn test_discovery_session() -> Result<(), Error> {
    let (client, server) = fidl::endpoints::create_proxy_and_stream::<HostMarker>();

    let address = Address::Public([0, 0, 0, 0, 0, 0]);
    let host = HostDevice::mock(HostId(1), address, "/dev/class/bt-hci/test".to_string(), client);
    let info_server = Arc::new(RwLock::new(host.info()));
    let server = Arc::new(RwLock::new(server));
    let _delegate = expect_set_bonding_delegate(server.clone()).await?;

    // Simulate request to establish discovery session
    let discovery_proxy = host.start_discovery()?;
    let mut discovery_request_stream: Option<DiscoverySessionRequestStream> = None;
    let expect_fidl = expect_call(server.clone(), |_, request| match request {
        HostRequest::StartDiscovery { payload, .. } => {
            info_server.write().discovering = true;
            discovery_request_stream = Some(payload.token.unwrap().into_stream());
            Ok(())
        }
        _ => Err(format_err!("Unexpected!")),
    });

    let _ = expect_fidl.await.expect("FIDL result unsatisfied");
    let discovery_request_stream = discovery_request_stream.unwrap();

    // Assert that host is now marked as discovering
    refresh_host(host.clone(), server.clone(), info_server.read().clone()).await;
    let is_discovering = host.info().discovering.clone();
    assert!(is_discovering);

    std::mem::drop(discovery_proxy);
    discovery_request_stream.map(|_| ()).collect::<()>().await;
    info_server.write().discovering = false;

    // Assert that host is no longer marked as discovering
    refresh_host(host.clone(), server.clone(), info_server.read().clone()).await;
    let is_discovering = host.info().discovering.clone();
    assert!(!is_discovering);

    Ok(())
}

// Create a HostDevice with a fake channel, restore bonds, check that the FIDL method is called
// over multiple paginated iterations
#[fuchsia::test(allow_stalls = false)]
async fn host_device_restore_bonds() -> Result<(), Error> {
    let (client, host_request_stream) = fidl::endpoints::create_proxy_and_stream::<HostMarker>();
    let address = Address::Public([0, 0, 0, 0, 0, 0]);
    let host = HostDevice::mock(HostId(1), address, "/dev/class/bt-hci/test".to_string(), client);
    let mut fake_host_server =
        FakeHostServer::new(host_request_stream, Arc::new(RwLock::new(host.info())));

    // TODO(https://fxbug.dev/42160922): Assume that 256 bonds is enough to cause HostDevice to use multiple
    // FIDL calls to transmit all of the bonds (at this time, the maximum message size is 64 KiB
    // and a fully-populated BondingData without peer service records is close to 700 B).
    let bonds = (0..=255).map(|i| {
        example::bond(Address::Public([i, i, i, i, i, i]), Address::Public([i, i, i, i, i, i]))
    });
    let restore_bonds = host.restore_bonds(bonds.collect());

    {
        let run_server = fake_host_server.run().fuse();
        pin_mut!(run_server);
        let result = futures::future::select(restore_bonds.fuse(), run_server).await;
        assert!(matches!(result, Either::Left { .. }));
    }
    assert_gt!(fake_host_server.num_restore_bonds_calls, 1);
    Ok(())
}

// TODO(https://fxbug.dev/42115226): Add host.fidl emulation to bt-fidl-mocks and use that instead.
async fn expect_call<F, D>(stream: Arc<RwLock<HostRequestStream>>, f: F) -> Result<D, Error>
where
    F: FnOnce(Arc<HostControlHandle>, HostRequest) -> Result<D, Error>,
{
    let control_handle = Arc::new(stream.read().control_handle());
    let mut stream = stream.write();
    if let Some(event) = stream.next().await {
        let event = event?;
        f(control_handle, event)
    } else {
        Err(format_err!("No event received"))
    }
}

async fn expect_set_bonding_delegate(
    stream: Arc<RwLock<HostRequestStream>>,
) -> Result<BondingDelegateRequestStream, Error> {
    expect_call(stream, |_, e| match e {
        HostRequest::SetBondingDelegate { delegate, .. } => Ok(delegate.into_stream()),
        _ => Err(format_err!("Unexpected!")),
    })
    .await
}

// Updates host with new info
async fn refresh_host(host: HostDevice, server: Arc<RwLock<HostRequestStream>>, info: HostInfo) {
    let refresh = host.refresh_test_host_info();
    let expect_fidl = expect_call(server, |_, e| match e {
        HostRequest::WatchState { responder } => {
            responder.send(&FidlHostInfo::from(info))?;
            Ok(())
        }
        _ => Err(format_err!("Unexpected!")),
    });

    let (refresh_result, expect_result) = join!(refresh, expect_fidl);
    let _ = refresh_result.expect("did not receive HostInfo update");
    let _ = expect_result.expect("FIDL result unsatisfied");
}
