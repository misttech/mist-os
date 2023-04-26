// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This test logs its own message at TRACE severity and then waits for it to be observable over
//! ArchiveAccessor. The key bit we're testing here is the BUILD.gn's test_specs.min_log_severity,
//! which should enable the lower level logging which is disabled by default. Without having that
//! flag correctly passed through test metadata to ffx/run-test-suite, this test should fail.

use diagnostics_reader::{ArchiveReader, Logs, Severity};
use fuchsia_async::{Task, Timer};
use futures::StreamExt;
use std::time::Duration;
use tracing::{info, trace};

#[fuchsia::test]
async fn hello_world() {
    info!("spawning task to write TRACE messages");
    Task::spawn(async {
        loop {
            trace!("TRACE LEVEL MESSAGE");
            Timer::new(Duration::from_secs(1)).await;
        }
    })
    .detach();

    let mut logs =
        ArchiveReader::new().snapshot_then_subscribe::<Logs>().unwrap().map(|r| r.unwrap());

    info!("checking for the TRACE messages our task is writing");
    while let Some(next) = logs.next().await {
        if next.metadata.severity == Severity::Trace && next.msg().unwrap() == "TRACE LEVEL MESSAGE"
        {
            break;
        }
        info!("still waiting for our own TRACE message");
    }
    info!("trace message encountered, test metadata successfully configured test realm");
}
