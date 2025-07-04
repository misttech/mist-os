// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_assertions::{assert_data_tree, AnyProperty};
use diagnostics_reader::{ArchiveReader, Severity};
use fidl::endpoints::ClientEnd;
use fuchsia_component_test::ScopedInstance;
use futures::{future, StreamExt};
use log::debug;
use {fidl_fuchsia_component as fcomponent, fuchsia_async as fasync};

const URL: &'static str =
    "fuchsia-pkg://fuchsia.com/test-manager-diagnostics-tests#meta/diagnostics-publisher.cm";

#[fuchsia::test]
async fn test_isolated_diagnostics_can_be_read_by_the_test() {
    debug!("I'm a debug log from a test");
    let mut instance =
        ScopedInstance::new("coll".into(), URL.into()).await.expect("Created instance");

    let _: ClientEnd<fcomponent::BinderMarker> = instance
        .connect_to_protocol_at_exposed_dir()
        .expect("failed to connect fuchsia.component.Binder");

    // Read inspect
    let data = ArchiveReader::inspect()
        .add_selector(r#"coll\:auto-*:root"#)
        .snapshot()
        .await
        .expect("got inspect data");
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].moniker.to_string(), format!(r#"coll:{}"#, instance.child_name()));
    assert_data_tree!(data[0].payload.as_ref().unwrap(), root: {
        "fuchsia.inspect.Health": {
            status: "OK",
            start_timestamp_nanos: AnyProperty,
        }
    });

    // Read logs
    let (subscription, mut errors) =
        ArchiveReader::logs().snapshot_then_subscribe().expect("subscribed").split_streams();
    fasync::Task::spawn(async move {
        if let Some(error) = errors.next().await {
            panic!("Got error: {:?}", error);
        }
    })
    .detach();
    let logs_fut = async move {
        let logs = subscription
            .filter(|log| future::ready(log.severity() == Severity::Info))
            .take(2)
            .collect::<Vec<_>>()
            .await;
        let messages =
            logs.into_iter().map(|log| log.msg().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!(
            messages,
            vec!["Started diagnostics publisher".to_owned(), "Finishing through Stop".to_owned()]
        );
    };

    let destroy_fut = instance.take_destroy_waiter();
    drop(instance);
    let () = destroy_fut.await.expect("failed to destroy instance");

    let () = logs_fut.await;
}
