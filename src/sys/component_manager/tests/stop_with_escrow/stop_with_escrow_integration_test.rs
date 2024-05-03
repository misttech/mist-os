// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use component_events::{
    events::{ExitStatus, Started, Stopped},
    matcher::EventMatcher,
};
use fidl_fidl_test_components::TriggerMarker;
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_process as fprocess;
use fuchsia_component_test::{RealmBuilder, RealmBuilderParams, ScopedInstanceFactory};
use fuchsia_zircon as zx;

/// Tests a component can stop with a request buffered in its outgoing dir,
/// and that request is handled on the next start (which should be automatic).
#[fuchsia::test]
async fn stop_with_pending_request() {
    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new().from_relative_url("#meta/test_root.cm"),
    )
    .await
    .unwrap();
    let instance =
        builder.build_in_nested_component_manager("#meta/component_manager.cm").await.unwrap();

    // Start the component with an eventpair `USER_0` handle for synchronization.
    let realm = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fcomponent::RealmMarker>()
        .expect("failed to connect to RealmQuery");
    let factory = ScopedInstanceFactory::new("coll").with_realm_proxy(realm);
    let instance = factory
        .new_named_instance("stop_with_pending_request", "#meta/stop_with_pending_request.cm")
        .await
        .unwrap();

    let (user_0, user_0_peer) = zx::EventPair::create();
    let execution_controller = instance
        .start_with_args(fcomponent::StartChildArgs {
            numbered_handles: Some(vec![fprocess::HandleInfo {
                handle: user_0_peer.into(),
                id: fuchsia_runtime::HandleType::User0 as u32,
            }]),
            ..Default::default()
        })
        .await
        .unwrap();

    // Queue request.
    let trigger = instance.connect_to_protocol_at_exposed_dir::<TriggerMarker>().unwrap();
    let call = trigger.run().check().unwrap();

    // Tell the component to stop.
    drop(user_0);

    // Observe that the component has stopped.
    let stop_payload = execution_controller.wait_for_stop().await.unwrap();
    assert_eq!(stop_payload.status.unwrap(), 0);

    // Observe that the component is started without the numbered handle and thus
    // handled our request.
    assert_eq!(&call.await.unwrap(), "hello");
}

/// Tests a component can stop with a request buffered in the framework waiting
/// for the server endpoint, and that request is handled when we write to the
/// client endpoint, which causes the component to be automatically started.
#[fuchsia::test]
async fn stop_with_delivery_on_readable_request() {
    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new().from_relative_url("#meta/test_root.cm"),
    )
    .await
    .unwrap();
    let instance =
        builder.build_in_nested_component_manager("#meta/component_manager.cm").await.unwrap();

    // Start the component and send it a connection request, but no messages
    // on the connection.
    let realm = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fcomponent::RealmMarker>()
        .expect("failed to connect to RealmQuery");
    let factory = ScopedInstanceFactory::new("coll").with_realm_proxy(realm);
    let instance = factory
        .new_named_instance(
            "stop_with_delivery_on_readable_request",
            "#meta/stop_with_delivery_on_readable_request.cm",
        )
        .await
        .unwrap();
    let execution_controller = instance.start().await.unwrap();
    let trigger = instance.connect_to_protocol_at_exposed_dir::<TriggerMarker>().unwrap();

    // Cause the framework to deliver the request to the component.
    assert_eq!(&trigger.run().await.unwrap(), "hello");

    // Wait for the component to stop because the connection stalled.
    let stop_payload = execution_controller.wait_for_stop().await.unwrap();
    assert_eq!(stop_payload.status.unwrap(), 0);

    // Now make a two-way call on the connection again and it should still work
    // (by starting the component again).
    assert_eq!(&trigger.run().await.unwrap(), "hello");
}

/// Tests a component can stop with an escrowed dictionary capability,
/// which will be read back on next start and used to maintain a call counter.
#[fuchsia::test]
async fn stop_with_escrowed_dictionary() {
    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new().from_relative_url("#meta/test_root.cm"),
    )
    .await
    .unwrap();
    let instance =
        builder.build_in_nested_component_manager("#meta/component_manager.cm").await.unwrap();
    let event_stream = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fcomponent::EventStreamMarker>()
        .unwrap();
    event_stream.wait_for_ready().await.unwrap();
    let mut event_stream = component_events::events::EventStream::new(event_stream);

    // Create the component.
    let realm = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fcomponent::RealmMarker>()
        .expect("failed to connect to RealmQuery");
    let factory = ScopedInstanceFactory::new("coll").with_realm_proxy(realm);
    let instance = factory
        .new_named_instance(
            "stop_with_escrowed_dictionary",
            "#meta/stop_with_escrowed_dictionary.cm",
        )
        .await
        .unwrap();

    // Send first request.
    let trigger = instance.connect_to_protocol_at_exposed_dir::<TriggerMarker>().unwrap();
    assert_eq!(trigger.run().await.unwrap(), "1");

    // Observe that the component has started then stopped.
    EventMatcher::ok()
        .moniker(instance.moniker())
        .wait::<Started>(&mut event_stream)
        .await
        .expect("failed to observe Started event");
    let stopped = EventMatcher::ok()
        .moniker(instance.moniker())
        .wait::<Stopped>(&mut event_stream)
        .await
        .expect("failed to observe Stopped event");
    assert_eq!(stopped.result().unwrap().status, ExitStatus::Clean);

    // Send second request, which should start the component again and continue the counter.
    assert_eq!(trigger.run().await.unwrap(), "2");
}
