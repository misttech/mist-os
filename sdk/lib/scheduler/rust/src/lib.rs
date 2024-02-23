// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error},
    fidl_fuchsia_scheduler::{ProfileProviderMarker, ProfileProviderSynchronousProxy},
    fuchsia_component::client::connect_to_protocol_sync,
    fuchsia_runtime,
    fuchsia_sync::RwLock,
    fuchsia_zircon::{HandleBased, Rights, Status, Thread, Time},
    std::sync::Arc,
};

static PROFILE_PROVIDER: RwLock<Option<Arc<ProfileProviderSynchronousProxy>>> = RwLock::new(None);

fn connect() -> Result<Arc<ProfileProviderSynchronousProxy>, Error> {
    // If the proxy has been connected already, return.
    if let Some(ref proxy) = *PROFILE_PROVIDER.read() {
        return Ok(Arc::clone(&proxy));
    }

    // Acquire the write lock and make sure no other thread connected to the proxy while we
    // were waiting on the write lock.
    let mut proxy = PROFILE_PROVIDER.write();
    if let Some(ref proxy) = *proxy {
        return Ok(Arc::clone(&proxy));
    }

    // Connect to the synchronous proxy.
    let p = Arc::new(connect_to_protocol_sync::<ProfileProviderMarker>()?);
    *proxy = Some(Arc::clone(&p));
    Ok(p)
}

fn disconnect() {
    // Drop our connection to the proxy by setting it to None.
    let mut proxy = PROFILE_PROVIDER.write();
    *proxy = None;
}

pub fn set_role_for_thread(thread: &Thread, role_name: &str) -> Result<(), Error> {
    let provider = connect()?;
    let thread = thread
        .duplicate_handle(Rights::SAME_RIGHTS)
        .context("Failed to duplicate thread handle")?;
    provider
        .set_profile_by_role(thread.into(), role_name, Time::INFINITE)
        .context("fuchsia.scheduler.SetProfileByRole failed")
        .and_then(|status| {
            // If the server responded with ZX_ERR_PEER_CLOSED, mark the synchronous proxy as
            // disconnected so future invocations of this function reconnect to the ProfileProvider.
            if status == Status::PEER_CLOSED.into_raw() {
                disconnect();
            }
            Status::ok(status)
                .context(format!("fuchsia.scheduler.SetProfileByRole returned error: {:?}", status))
        })?;
    Ok(())
}

pub fn set_role_for_this_thread(role_name: &str) -> Result<(), Error> {
    set_role_for_thread(&fuchsia_runtime::thread_self(), role_name)
}
