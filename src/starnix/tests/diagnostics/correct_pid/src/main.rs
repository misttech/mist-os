// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use component_events::{
    events::{EventStream, ExitStatus, Stopped},
    matcher::EventMatcher,
};
use diagnostics_reader::{ArchiveReader, Inspect, Logs};
use fidl_fuchsia_component::BinderMarker;
use fuchsia_component::client::connect_to_protocol;
use futures::StreamExt;
use parse_starnix_inspect::CoredumpReport;
use tracing::info;

#[fuchsia::main]
async fn main() {
    info!("running hello_world...");
    let mut event_stream = EventStream::open().await.unwrap();

    connect_to_protocol::<BinderMarker>().unwrap();
    assert_eq!(
        EventMatcher::ok()
            .moniker("kmsg_hello_world")
            .wait::<Stopped>(&mut event_stream)
            .await
            .unwrap()
            .result()
            .unwrap()
            .status,
        // Our Linux program requests a SIGABRT so there's a coredump report in inspect.
        ExitStatus::Crash(11),
    );

    let kernel_inspect = ArchiveReader::new()
        .select_all_for_moniker("kernel")
        .with_minimum_schema_count(1)
        .retry_if_empty(true)
        .snapshot::<Inspect>()
        .await
        .unwrap()[0]
        .clone();
    let coredumps = CoredumpReport::extract_from_snapshot(&kernel_inspect).unwrap();
    assert_eq!(
        coredumps.len(),
        1,
        "exactly one task should have ran and crashed in the kernel, saw {coredumps:?}"
    );
    assert_eq!(coredumps[0].argv, "data/tests/linux_kmsg_hello_world");

    let hello_world_pid = coredumps[0].process_koid;
    let hello_world_tid = coredumps[0].thread_koid;

    let mut logs = ArchiveReader::new().snapshot_then_subscribe::<Logs>().unwrap();
    let hello_world_msg = loop {
        let next = logs.next().await.unwrap().unwrap();
        if next.msg() == Some("Hello, Starnix logs!") {
            break next;
        }
    };
    assert_eq!(hello_world_msg.metadata.pid.unwrap(), hello_world_pid);
    assert_eq!(hello_world_msg.metadata.tid.unwrap(), hello_world_tid);
}
