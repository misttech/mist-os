// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Result};
use discovery::{
    wait_for_devices, DiscoverySources, FastbootConnectionState, TargetEvent, TargetFilter,
    TargetState,
};
use futures::StreamExt;

pub(crate) async fn rediscover_helper<F, U>(
    target_name: &String,
    filter: F,
    cb: &mut U,
) -> Result<()>
where
    F: TargetFilter,
    U: FnMut(FastbootConnectionState) -> Result<()>,
{
    let mut device_stream =
        wait_for_devices(filter, true, false, DiscoverySources::MDNS | DiscoverySources::MANUAL)
            .await?;

    if let Some(Ok(event)) = device_stream.next().await {
        // This is the first event that matches our filter.
        // Mutate our internal understanding of the address
        // the target is at with the new address discovered
        match event {
            TargetEvent::Removed(_) => {
                bail!("When rediscovering target: {}, expected a target Added event but got a Removed event", target_name)
            }
            TargetEvent::Added(handle) => match handle.state {
                TargetState::Fastboot(ts) => cb(ts.connection_state)?,
                state @ _ => bail!("When rediscovering target: {}, expected target to be rediscovered in Fastboot mode. Got: {}", target_name, state),
            },
        }
        return Ok(());
    }
    Ok(())
}
