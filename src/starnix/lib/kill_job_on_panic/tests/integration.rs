// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use component_events::{
    events::{EventStream, ExitStatus, Stopped},
    matcher::EventMatcher,
};
use diagnostics_reader::{ArchiveReader, Logs, Severity};
use fidl_fuchsia_component::BinderMarker;
use fuchsia_component::client::connect_to_protocol;
use futures::StreamExt;
use tracing::info;

#[fuchsia::test]
async fn panicking_process_kills_job_with_other_processes() {
    let mut events = EventStream::open().await.unwrap();

    info!("starting panicking_child");
    let _start_panicking_child = connect_to_protocol::<BinderMarker>().unwrap();

    // Wait for component stop event to arrive. The component will only be stopped by ELF runner
    // when the "root" process terminates, which in this test will only happen when its job is
    // killed by the kill_job_on_panic library called by the child process it spawns. The test
    // should time out if the hook does not work.
    info!("waiting for panicking_child to stop");
    assert_eq!(
        EventMatcher::ok()
            .moniker("panicking_child")
            .wait::<Stopped>(&mut events)
            .await
            .unwrap()
            .result()
            .unwrap()
            .status,
        ExitStatus::Crash(11),
    );

    info!("panicking_child stopped, checking for expected error message");
    let mut logs = ArchiveReader::new().snapshot_then_subscribe::<Logs>().unwrap();
    loop {
        let next_log = logs
            .next()
            .await
            .expect("must encounter expected message before logs end")
            .expect("should not receive errors from archivist");

        if next_log.msg().unwrap() == "patricide" && next_log.metadata.severity == Severity::Error {
            break;
        }
    }
}
