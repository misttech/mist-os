// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, format_err, Context, Error},
    argh::FromArgs,
    fidl_fuchsia_fs_realm::ControllerMarker,
    fidl_fuchsia_sys2::RealmQueryMarker,
    fuchsia_component::client::{connect_to_protocol_at_dir_root, connect_to_protocol_at_path},
    fuchsia_zircon as zx,
    std::path::Path,
};

#[derive(FromArgs)]
#[argh(description = "A utility for unmounting a filesystem instance running on a block device.")]
struct Options {
    /// the path at which the filesystem instance was mounted on the block device
    #[argh(positional)]
    mount_path: String,
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let opt: Options = argh::from_env();
    let mount_path = Path::new(&opt.mount_path);
    // Make sure that path can be canonicalized and starts with mnt
    if let Some(parent_path) = mount_path.parent() {
        if parent_path.to_str() != Some("/mnt") {
            let canonical_parent_path = parent_path.canonicalize().context("Bad mount path")?;
            if canonical_parent_path.to_str() != Some("/mnt") {
                return Err(anyhow!("Only mounts in /mnt are supported."));
            }
        }
    } else {
        return Err(anyhow!("Only mounts with a parent path are supported"));
    }

    let filename = mount_path.file_name().ok_or(anyhow!("Failed to get file name"))?;
    let filename_str =
        filename.to_str().ok_or(anyhow!("Failed to convert filename from OsStr to str"))?;

    // Connect to fs_realm
    const REALM_QUERY_SERVICE_PATH: &str = "/svc/fuchsia.sys2.RealmQuery.root";
    const FS_REALM_MONIKER: &str = "./core/fs_realm";

    let realm_query_proxy =
        connect_to_protocol_at_path::<RealmQueryMarker>(REALM_QUERY_SERVICE_PATH)?;

    let resolved_dirs = realm_query_proxy
        .get_instance_directories(FS_REALM_MONIKER)
        .await?
        .map_err(|e| format_err!("RealmQuery error: {:?}", e))?
        .ok_or(format_err!("{} is not resolved", FS_REALM_MONIKER))?;
    let exposed_dir = resolved_dirs.exposed_dir.into_proxy()?;

    let fs_realm_proxy = connect_to_protocol_at_dir_root::<ControllerMarker>(&exposed_dir)
        .context("Failed to connect to fuchsia.fs.realm.Controller")?;
    fs_realm_proxy
        .unmount(filename_str)
        .await
        .context("Transport error on format")?
        .map_err(zx::Status::from_raw)
        .context("Failed to format block device")?;
    Ok(())
}
