// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    assert_matches::assert_matches,
    fidl::{endpoints::ProtocolMarker, OnSignals},
    fidl_fuchsia_test::*,
    fuchsia_zircon::Signals,
    realm_proxy::client::*,
};

#[fuchsia::test]
async fn realm_factory_can_create_test_realm() -> Result<(), Error> {
    let realm_factory = RealmFactoryClient::connect()?;
    let result = realm_factory.create_realm().await;

    assert_matches!(result.err(), None);

    Ok(())
}

#[fuchsia::test]
async fn realm_factory_can_handle_concurrent_realm_proxy_connections() -> Result<(), Error> {
    let realm_factory = RealmFactoryClient::connect()?;

    let realm1 = realm_factory.create_realm().await?;
    // This should not block on realm1.
    let realm2 = realm_factory.create_realm().await?;

    let exposed = realm1.connect_to_protocol::<ExposedMarker>().await?.into_proxy()?;
    assert_matches!(exposed.call().await.err(), None);
    drop(realm1); // This should not affect realm 2.

    let exposed = realm2.connect_to_protocol::<ExposedMarker>().await?.into_proxy()?;
    assert_matches!(exposed.call().await.err(), None);
    Ok(())
}

#[fuchsia::test]
async fn can_invoke_proxied_protocols() -> Result<(), Error> {
    let realm = RealmProxyClient::connect()?;
    let exposed = realm.connect_to_protocol::<ExposedMarker>().await?.into_proxy()?;

    assert_matches!(exposed.call().await.err(), None);

    Ok(())
}

#[fuchsia::test]
async fn cannot_invoke_unproxied_protocols() -> Result<(), Error> {
    let realm = RealmProxyClient::connect()?;
    let not_exposed = realm.connect_to_protocol::<NotExposedMarker>().await?.into_proxy()?;

    assert_matches!(
        not_exposed.call().await.err(),
        Some(fidl::Error::ClientChannelClosed {
            status: fuchsia_zircon::Status::NOT_FOUND,
            protocol_name: NotExposedMarker::DEBUG_NAME,
        })
    );

    Ok(())
}

#[fuchsia::test]
async fn test_realm_is_disposed_on_disconnect() -> Result<(), Error> {
    let (local, remote) = fidl::EventPair::create();

    {
        let realm = RealmProxyClient::connect()?;
        let event_holder =
            realm.connect_to_protocol::<EventPairHolderMarker>().await?.into_proxy()?;
        event_holder.hold(remote)?;
    }
    // RealmProxy connection is dropped here.

    assert_eq!(
        OnSignals::new(&local, Signals::CHANNEL_PEER_CLOSED).await? & Signals::CHANNEL_PEER_CLOSED,
        Signals::CHANNEL_PEER_CLOSED
    );

    Ok(())
}
