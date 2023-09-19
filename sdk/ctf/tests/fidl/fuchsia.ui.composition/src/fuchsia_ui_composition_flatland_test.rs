// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl;
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

#[fuchsia::test]
async fn test_flatland_connection_is_ok() -> Result<(), Error> {
    let (context_proxy, context_server_end) =
        fidl::endpoints::create_proxy::<ui_test_context::ContextMarker>()
            .expect("create Context proxy and server_end");

    let factory_proxy = client::connect_to_protocol::<ui_test_context::FactoryMarker>()
        .expect("connect to FactoryProxy");

    factory_proxy
        .create(ui_test_context::FactoryCreateRequest {
            context_server: Some(context_server_end),
            ..Default::default()
        })
        .expect("create Context");

    let (flatland_proxy, flatland_server_end) =
        fidl::endpoints::create_proxy::<flatland::FlatlandMarker>()
            .expect("create Flatland proxy and server_end");

    let mut flatland_event_stream = flatland_proxy.take_event_stream();

    context_proxy.connect_to_flatland(flatland_server_end).expect("connect to Flatland");

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
