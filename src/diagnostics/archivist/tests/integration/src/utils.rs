// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use component_events::{events::*, matcher::*};

pub(crate) async fn wait_for_component_to_crash(moniker: &str) {
    let mut event_stream = EventStream::open().await.unwrap();
    EventMatcher::ok()
        .stop(Some(ExitStatusMatcher::AnyCrash))
        .moniker(moniker)
        .wait::<Stopped>(&mut event_stream)
        .await
        .unwrap();
}

pub(crate) async fn wait_for_component_stopped_event(
    instance_child_name: &str,
    component: &str,
    status_match: ExitStatusMatcher,
    event_stream: &mut EventStream,
) {
    let moniker_for_match = format!("./realm_builder:{instance_child_name}/test/{component}");
    EventMatcher::ok()
        .stop(Some(status_match))
        .moniker(moniker_for_match)
        .wait::<Stopped>(event_stream)
        .await
        .unwrap();
}
