// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    constants::*,
    test_topology::{self, expose_test_realm_protocol},
    utils,
};
use component_events::{events::*, matcher::*};
use diagnostics_reader::{assert_data_tree, ArchiveReader, Data, Logs};
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_diagnostics::ArchiveAccessorMarker;
use fuchsia_async as fasync;
use fuchsia_component_test::ScopedInstanceFactory;
use futures::StreamExt;

const LOG_AND_EXIT_COMPONENT: &str = "log_and_exit";

#[fuchsia::test]
async fn test_logs_lifecycle() {
    let (builder, test_realm) = test_topology::create(test_topology::Options::default())
        .await
        .expect("create base topology");
    test_topology::add_collection(&test_realm, "coll").await.unwrap();

    // Currently RealmBuilder doesn't support to expose a capability from framework, therefore we
    // manually update the decl that the builder creates.
    expose_test_realm_protocol(&builder, &test_realm).await;
    let realm = builder.build().await.unwrap();
    let accessor =
        realm.root.connect_to_protocol_at_exposed_dir::<ArchiveAccessorMarker>().unwrap();

    let mut reader = ArchiveReader::new();
    reader
        .with_archive(accessor)
        .with_minimum_schema_count(0) // we want this to return even when no log messages
        .retry_if_empty(false);

    let (mut subscription, mut errors) =
        reader.snapshot_then_subscribe::<Logs>().unwrap().split_streams();
    let _log_errors = fasync::Task::spawn(async move {
        if let Some(error) = errors.next().await {
            panic!("{error:#?}");
        }
    });

    let moniker = format!("realm_builder:{}/test/coll:log_and_exit", realm.root.child_name());

    let mut event_stream = EventStream::open().await.unwrap();
    reader.retry_if_empty(true);
    for i in 1..50 {
        // launch our child, wait for it to exit, and destroy (so all its outgoing log connections
        // are processed) before asserting on its logs
        let realm_proxy =
            realm.root.connect_to_protocol_at_exposed_dir::<fcomponent::RealmMarker>().unwrap();
        let mut instance = ScopedInstanceFactory::new("coll")
            .with_realm_proxy(realm_proxy)
            .new_named_instance(LOG_AND_EXIT_COMPONENT, LOG_AND_EXIT_COMPONENT_URL)
            .await
            .unwrap();
        let _ = instance.connect_to_protocol_at_exposed_dir::<fcomponent::BinderMarker>().unwrap();

        utils::wait_for_component_stopped_event(
            realm.root.child_name(),
            &format!("coll:{LOG_AND_EXIT_COMPONENT}"),
            ExitStatusMatcher::Clean,
            &mut event_stream,
        )
        .await;

        check_message(&moniker, subscription.next().await.unwrap());

        reader.with_minimum_schema_count(i);
        let all_messages = reader.snapshot::<Logs>().await.unwrap();

        for message in all_messages {
            check_message(&moniker, message);
        }

        let waiter = instance.take_destroy_waiter();
        drop(instance);
        waiter.await.unwrap();
    }
}

fn check_message(expected_moniker: &str, message: Data<Logs>) {
    assert_eq!(message.moniker, expected_moniker,);
    assert_eq!(message.metadata.component_url, Some(LOG_AND_EXIT_COMPONENT_URL.to_string()));

    assert_data_tree!(message.payload.unwrap(), root: {
        message: {
            value: "Hello, world!",
        }
    });
}
