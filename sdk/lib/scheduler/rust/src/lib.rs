// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error},
    fidl_connector::{Connect, ServiceReconnector},
    fidl_fuchsia_scheduler::ProfileProviderMarker,
    fuchsia_runtime,
    fuchsia_zircon::{HandleBased, Rights, Status, Thread},
    std::sync::OnceLock,
};

// Statically initializes a fidl reconnector that will be used to maintain a persistent connection
// to the ProfileProvider service. This is static because we want successive calls to use the same
// reconnector.
fn profile_provider_connector() -> &'static ServiceReconnector<ProfileProviderMarker> {
    static PROFILE_PROVIDER_CONNECTOR: OnceLock<ServiceReconnector<ProfileProviderMarker>> =
        OnceLock::new();
    PROFILE_PROVIDER_CONNECTOR.get_or_init(|| ServiceReconnector::<ProfileProviderMarker>::new())
}

pub async fn set_role_for_thread(thread: &Thread, role_name: &str) -> Result<(), Error> {
    let provider = profile_provider_connector().connect()?;
    let thread = thread
        .duplicate_handle(Rights::SAME_RIGHTS)
        .context("Failed to duplicate thread handle")?;
    provider
        .set_profile_by_role(thread.into(), role_name)
        .await
        .context("fuchsia.scheduler.SetProfileByRole failed")
        .and_then(|status| {
            Status::ok(status).context("fuchsia.scheduler.SetProfileByRole returned error")
        })?;
    Ok(())
}

pub async fn set_role_for_this_thread(role_name: &str) -> Result<(), Error> {
    set_role_for_thread(&fuchsia_runtime::thread_self(), role_name).await
}
