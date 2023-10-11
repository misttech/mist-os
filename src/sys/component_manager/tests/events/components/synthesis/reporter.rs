// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    component_events::events::{DirectoryReady, Event, EventStream},
    fidl_fuchsia_component as fcomponent,
    fuchsia_component::server::ServiceFs,
    futures::FutureExt,
    futures::StreamExt,
};

#[fuchsia::main]
async fn main() {
    let mut fs = ServiceFs::new();
    fs.dir("diagnostics");
    fs.take_and_serve_directory_handle().unwrap();

    // wait for a DirectoryReady event to come over the event stream, then exit
    let directory_ready_waiter = fuchsia_async::Task::spawn(async {
        let mut event_stream = EventStream::open().await.unwrap();
        let mut found_directory_ready = false;
        loop {
            let event = event_stream.next().await.unwrap();
            if matches!(
                event,
                fcomponent::Event {
                    header: Some(fcomponent::EventHeader {
                        event_type: Some(DirectoryReady::TYPE),
                        ..
                    }),
                    ..
                }
            ) {
                found_directory_ready = true;
            }

            if found_directory_ready {
                break;
            }
        }
    });

    futures::select! {
        () = fs.collect::<()>().fuse() => panic!("filesystem exited unexpectedly"),
        () = directory_ready_waiter.fuse() => {},
    }
}
