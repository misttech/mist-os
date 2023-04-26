// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_reader::{ArchiveReader, Logs, Severity};
use fidl_fuchsia_component::{BinderMarker, EventStreamMarker, EventType};
use fuchsia_component::client::connect_to_protocol;
use futures::StreamExt;

#[fuchsia::test]
async fn panicking_process_kills_job_with_other_processes() {
    println!("starting panicking_child");
    let _start_panicking_child = connect_to_protocol::<BinderMarker>().unwrap();

    // Wait for component stop event to arrive. The component will only be stopped by ELF runner
    // when the "root" process terminates, which in this test will only happen when its job is
    // killed by the kill_job_on_panic library called by the child process it spawns. The test
    // should time out if the hook does not work.
    println!("waiting for panicking_child to stop");
    let realm_events = connect_to_protocol::<EventStreamMarker>().unwrap();
    'wait_for_stop: loop {
        for event in realm_events.get_next().await.unwrap() {
            let header = event.header.as_ref().expect("all events have a header");
            match (header.event_type, header.moniker.as_ref().map(|m| m.as_str())) {
                (Some(EventType::Stopped), Some("./panicking_child")) => {
                    break 'wait_for_stop;
                }
                _ => continue,
            }
        }
    }

    println!("panicking_child stopped, checking for expected error message");
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

    println!("got both component stop and expected log message, exiting");
}
