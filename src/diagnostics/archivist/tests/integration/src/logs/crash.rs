// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{constants::*, test_topology, utils};
use component_events::matcher::ExitStatusMatcher;
use diagnostics_reader::{assert_data_tree, ArchiveReader, Logs, Severity};
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_diagnostics::ArchiveAccessorMarker;
use fuchsia_async::Task;
use fuchsia_component_test::ScopedInstanceFactory;
use futures::prelude::*;

#[fuchsia::test]
async fn logs_from_crashing_component() {
    let (builder, test_realm) = test_topology::create(test_topology::Options::default())
        .await
        .expect("create base topology");
    test_topology::add_collection(&test_realm, "coll").await.unwrap();

    test_topology::expose_test_realm_protocol(&builder, &test_realm).await;
    let realm = builder.build().await.unwrap();
    let realm_proxy =
        realm.root.connect_to_protocol_at_exposed_dir::<fcomponent::RealmMarker>().unwrap();
    let mut instance = ScopedInstanceFactory::new("coll")
        .with_realm_proxy(realm_proxy)
        .new_named_instance("log_and_crash", LOG_AND_CRASH_COMPONENT_URL)
        .await
        .unwrap();

    let accessor =
        realm.root.connect_to_protocol_at_exposed_dir::<ArchiveAccessorMarker>().unwrap();
    let mut reader = ArchiveReader::new();
    reader.with_archive(accessor);
    let (mut logs, mut errors) = reader.snapshot_then_subscribe::<Logs>().unwrap().split_streams();
    let _errors = Task::spawn(async move {
        while let Some(e) = errors.next().await {
            panic!("error in subscription: {e}");
        }
    });

    // launch our child, wait for it to exit, and destroy (so all its outgoing log connections
    // are processed) before asserting on its logs
    let _ = instance.connect_to_protocol_at_exposed_dir::<fcomponent::BinderMarker>().unwrap();

    utils::wait_for_component_stopped(
        realm.root.child_name(),
        "coll:log_and_crash",
        ExitStatusMatcher::AnyCrash,
    )
    .await;
    let waiter = instance.take_destroy_waiter();
    drop(instance);
    waiter.await.unwrap();

    let crasher_info = logs.next().await.unwrap();
    assert_eq!(crasher_info.metadata.severity, Severity::Info);
    assert_data_tree!(crasher_info.payload.unwrap(), root:{"message": contains {
        "value": "crasher has initialized",
    }});

    let crasher_warn = logs.next().await.unwrap();
    assert_eq!(crasher_warn.metadata.severity, Severity::Warn);
    assert_data_tree!(crasher_warn.payload.unwrap(), root:{"message": contains {
        "value": "crasher is approaching the crash",
    }});

    let crasher_error = logs.next().await.unwrap();
    assert_eq!(crasher_error.metadata.severity, Severity::Error);
    assert_data_tree!(crasher_error.payload.unwrap(), root:{"message": contains {
        "value": "oh no we're crashing",
    }});
}
