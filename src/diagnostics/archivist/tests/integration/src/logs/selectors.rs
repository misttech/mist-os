// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{constants, test_topology, utils};
use component_events::{events::*, matcher::*};
use diagnostics_reader::{assert_data_tree, ArchiveReader, Logs};
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_diagnostics::ArchiveAccessorMarker;
use fuchsia_async as fasync;
use fuchsia_component_test::{RealmInstance, ScopedInstanceFactory};
use futures::{FutureExt, StreamExt};

#[fuchsia::test]
async fn component_selectors_filter_logs() {
    let (builder, test_realm) = test_topology::create(test_topology::Options::default())
        .await
        .expect("create base topology");
    test_topology::add_collection(&test_realm, "coll").await.unwrap();

    test_topology::expose_test_realm_protocol(&builder, &test_realm).await;
    let realm = builder.build().await.expect("create instance");
    let accessor =
        realm.root.connect_to_protocol_at_exposed_dir::<ArchiveAccessorMarker>().unwrap();

    let mut event_stream = EventStream::open().await.unwrap();

    // Start a few components.
    for _ in 0..3 {
        launch_and_wait_for_exit(&realm, "a", &mut event_stream).await;
        launch_and_wait_for_exit(&realm, "b", &mut event_stream).await;
    }

    // Start listening
    let mut reader = ArchiveReader::new();
    reader
        .add_selector("coll\\:a:root")
        .with_archive(accessor)
        .with_minimum_schema_count(5)
        .retry_if_empty(true);

    let (mut stream, mut errors) =
        reader.snapshot_then_subscribe::<Logs>().unwrap().split_streams();
    let _errors = fasync::Task::spawn(async move {
        while let Some(e) = errors.next().await {
            panic!("error in subscription: {e}");
        }
    });

    // Start a few more components
    for _ in 0..3 {
        launch_and_wait_for_exit(&realm, "a", &mut event_stream).await;
        launch_and_wait_for_exit(&realm, "b", &mut event_stream).await;
    }

    // We should see logs from components started before and after we began to listen.
    for _ in 0..6 {
        let log = stream.next().await.unwrap();
        assert_eq!(log.moniker, "coll:a");
        assert_data_tree!(log.payload.unwrap(), root: {
            message: {
                value: "Hello, world!",
            }
        });
    }
    // We only expect 6 logs.
    assert!(stream.next().now_or_never().is_none());
}

async fn launch_and_wait_for_exit(
    realm: &RealmInstance,
    name: &str,
    event_stream: &mut EventStream,
) {
    // launch our child, wait for it to exit, and destroy (so all its outgoing log connections
    // are processed) before asserting on its logs
    let realm_proxy =
        realm.root.connect_to_protocol_at_exposed_dir::<fcomponent::RealmMarker>().unwrap();
    let mut instance = ScopedInstanceFactory::new("coll")
        .with_realm_proxy(realm_proxy)
        .new_named_instance(name, constants::LOG_AND_EXIT_COMPONENT_URL)
        .await
        .unwrap();
    let _ = instance.connect_to_protocol_at_exposed_dir::<fcomponent::BinderMarker>().unwrap();

    utils::wait_for_component_stopped_event(
        realm.root.child_name(),
        &format!("coll:{name}"),
        ExitStatusMatcher::Clean,
        event_stream,
    )
    .await;
    let waiter = instance.take_destroy_waiter();
    drop(instance);
    waiter.await.unwrap();
}
