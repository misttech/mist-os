// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl;
use fidl_fuchsia_testing_harness as harness;
use fidl_fuchsia_ui_composition as flatland;
use fidl_fuchsia_ui_test_context as ui_test_context;
use flatland::PresentArgs;
use fuchsia_component::client;
use futures::StreamExt;

const TRANSFORM_ID: flatland::TransformId = flatland::TransformId { value: 3 };

/// This test uses conformance test pattern for
/// fuchsia.ui.composition.Flatland protocol smoke test.
///
/// The test leverages fuchsia.ui.test.context.Factory capability
/// to connect with fuchsia.ui.composition.Flatland protocol.

async fn connect_to_realm() -> Result<harness::RealmProxy_Proxy, Error> {
    let (realm_proxy, realm_server) = fidl::endpoints::create_proxy::<harness::RealmProxy_Marker>()
        .expect("create realm_proxy and realm_server");
    let realm_factory_proxy = client::connect_to_protocol::<ui_test_context::RealmFactoryMarker>()
        .expect("connect to realm factory proxy");
    let _ = realm_factory_proxy
        .create_realm(ui_test_context::RealmFactoryCreateRealmRequest {
            realm_server: Some(realm_server),
            ..Default::default()
        })
        .await
        .expect("create realm");

    Ok(realm_proxy)
}

async fn connect_to_protocol<T: fidl::endpoints::DiscoverableProtocolMarker>(
    proxy: &harness::RealmProxy_Proxy,
) -> Result<T::Proxy, Error> {
    let (client, server) = fidl::endpoints::create_endpoints::<T>();
    let _ = proxy
        .connect_to_named_protocol(T::PROTOCOL_NAME, server.into_channel())
        .await
        .expect("connect_to");

    Ok(client.into_proxy()?)
}

#[fuchsia::test]
async fn test_flatland_connection_is_ok() -> Result<(), Error> {
    let realm_proxy = connect_to_realm().await.expect("connect_to_realm");

    let flatland_proxy = connect_to_protocol::<flatland::FlatlandMarker>(&realm_proxy).await?;
    let mut flatland_event_stream = flatland_proxy.take_event_stream();

    flatland_proxy.create_transform(&TRANSFORM_ID).expect("create transform");

    flatland_proxy.present(PresentArgs::default()).expect("flatland present");

    let ofp = flatland_event_stream
        .next()
        .await
        .unwrap()
        .expect("get FlatlandEvent")
        .into_on_frame_presented();
    assert!(ofp.is_some());
    Ok(())
}
