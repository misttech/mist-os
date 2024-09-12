// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use component_debug::dirs::*;
use component_debug::query::get_single_instance_from_query;
use fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd};
use fidl_fuchsia_starnix_container::{ControllerMarker, ControllerProxy};
use fuchsia_component::client::connect_to_protocol_at_path;
use {fidl_fuchsia_io as fio, fidl_fuchsia_sys2 as fsys, fuchsia_zircon as zx};

async fn find_moniker(moniker: Option<String>) -> Result<String> {
    if let Some(moniker) = moniker {
        return Ok(moniker);
    }
    return Ok("".to_owned());
}

async fn connect_to_capability_in_dir(
    dir: &fio::DirectoryProxy,
    capability_name: &str,
    server_end: zx::Channel,
    flags: fio::OpenFlags,
) -> Result<()> {
    check_entry_exists(dir, capability_name).await?;

    // Connect to the capability
    dir.open(flags, fio::ModeType::empty(), capability_name, ServerEnd::new(server_end))?;
    Ok(())
}

// Checks that the given directory contains an entry with the given name.
async fn check_entry_exists(dir: &fio::DirectoryProxy, capability_name: &str) -> Result<()> {
    let dir_idx: Option<usize> = capability_name.rfind('/');
    let (capability_name, entries) = match dir_idx {
        Some(dir_idx) => {
            let dirname = &capability_name[0..dir_idx];
            let basename = &capability_name[dir_idx + 1..];
            let nested_dir = fuchsia_fs::directory::open_directory_deprecated(
                dir,
                dirname,
                fio::OpenFlags::RIGHT_READABLE,
            )
            .await?;
            let entries = fuchsia_fs::directory::readdir(&nested_dir).await?;
            (basename, entries)
        }
        None => {
            let entries = fuchsia_fs::directory::readdir(dir).await?;
            (capability_name, entries)
        }
    };
    if entries.iter().any(|e| e.name == capability_name) {
        Ok(())
    } else {
        Err(anyhow!("Error: Capability '{}' not found.", capability_name))
    }
}

pub async fn connect_to_contoller(moniker: Option<String>) -> Result<ControllerProxy> {
    let moniker = find_moniker(moniker).await?;

    // Connect to the root RealmQuery protocol
    let realm_query =
        connect_to_protocol_at_path::<fsys::RealmQueryMarker>("/svc/fuchsia.sys2.RealmQuery.root")?;

    let instance = get_single_instance_from_query(&moniker, &realm_query).await?;

    println!("Moniker: {}", instance.moniker);

    let (proxy, server_end) =
        fidl::endpoints::create_proxy::<ControllerMarker>().context("failed to create proxy")?;

    let dir = open_instance_dir_root_readable(
        &instance.moniker,
        fsys::OpenDirType::ExposedDir.into(),
        &realm_query,
    )
    .await?;

    connect_to_capability_in_dir(
        &dir,
        ControllerMarker::PROTOCOL_NAME,
        server_end.into_channel(),
        fio::OpenFlags::empty(),
    )
    .await?;

    Ok(proxy)
}
