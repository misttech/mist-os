// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    component_events::{
        events::{DirectoryReady, Event, EventStream},
        matcher::EventMatcher,
    },
    fidl::endpoints::{create_endpoints, ClientEnd, DiscoverableProtocolMarker as _},
    fidl_fidl_test_components as ftest, fidl_fuchsia_io as fio, fuchsia_fs,
    futures::StreamExt,
    maplit::hashmap,
};

async fn list_entries(directory: &fio::DirectoryProxy) -> Vec<String> {
    fuchsia_fs::directory::readdir_recursive(&directory, /*timeout=*/ None)
        .map(|entry_result| entry_result.expect("entry ok").name)
        .collect::<Vec<_>>()
        .await
}

async fn call_trigger(directory: &fio::DirectoryProxy, paths: &Vec<String>) {
    for path in paths {
        let trigger = fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
            ftest::TriggerMarker,
        >(directory, path)
        .expect("open dir");
        // We're only interested in this function successfully returning, we don't care about the
        // contents of the string returned.
        let _: String = trigger.run().await.expect("call trigger");
    }
}

/// This component receives `DirectoryReady` events when its child makes them available.
/// Those directories contain a `Trigger` service that should be accessible when opening the
/// directory.
/// It sends "Saw: /path/to/dir on /some_moniker" for each successful read.
#[fuchsia::main]
async fn main() {
    let mut event_stream = EventStream::open().await.unwrap();
    // For successful DirectoryReady events, this is a map of the directory to expected contents
    let mut all_expected_entries = hashmap! {
        "normal".to_string() => vec![ftest::TriggerMarker::PROTOCOL_NAME.to_string()],
        "nested".to_string() => vec![format!("inner/{}", ftest::TriggerMarker::PROTOCOL_NAME).to_string()],
    };

    for _ in 0..2 {
        let event = EventMatcher::ok().expect_match::<DirectoryReady>(&mut event_stream).await;
        assert_eq!(event.target_moniker(), "./child");
        let payload = event.result().unwrap();

        let expected_entries = all_expected_entries.remove(&payload.name).unwrap();

        // Open the directory and verify its contents
        let (node_clone, server_end) = create_endpoints();
        payload.node.clone(fio::OpenFlags::CLONE_SAME_RIGHTS, server_end).unwrap();
        let directory = ClientEnd::<fio::DirectoryMarker>::new(node_clone.into_channel());
        let directory = directory.into_proxy().unwrap();

        let entries = list_entries(&directory).await;

        assert_eq!(entries, expected_entries);

        // Call the trigger service on each expected entry
        call_trigger(&directory, &expected_entries).await;
    }
}
