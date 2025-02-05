// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use std::sync::Arc;
use vfs::directory::entry::DirectoryEntry;
use {
    fidl_fuchsia_developer_remotecontrol as rc, fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as sys2,
};

pub async fn toolbox_directory(
    remote_proxy: &rc::RemoteControlProxy,
    query: &sys2::RealmQueryProxy,
) -> Result<Arc<impl DirectoryEntry>> {
    let controller =
        rcs::root_lifecycle_controller(remote_proxy, std::time::Duration::from_secs(5)).await?;
    // Attempt to resolve both the modern and legacy locations concurrently and use the one that
    // resolves successfully
    let moniker = moniker::Moniker::try_from("toolbox")?;
    let legacy_moniker = moniker::Moniker::try_from("core/toolbox")?;
    let (modern, legacy) = futures::join!(
        component_debug::lifecycle::resolve_instance(&controller, &moniker),
        component_debug::lifecycle::resolve_instance(&controller, &legacy_moniker)
    );

    let moniker = if modern.is_ok() {
        moniker
    } else if legacy.is_ok() {
        legacy_moniker
    } else {
        return Err(anyhow::anyhow!(
            "Unable to resolve toolbox component in either toolbox or core/toolbox"
        ));
    };

    let dir = component_debug::dirs::open_instance_dir_root_readable(
        &moniker,
        sys2::OpenDirType::NamespaceDir.into(),
        &query,
    )
    .await?;

    let svc_dir = fuchsia_fs::directory::open_directory(&dir, "svc", fio::PERM_READABLE).await?;
    Ok(vfs::remote::remote_dir(svc_dir))
}
