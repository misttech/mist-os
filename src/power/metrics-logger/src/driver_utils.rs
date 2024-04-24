// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Result},
    fidl_fuchsia_device as fdevice, fuchsia_zircon as zx,
    tracing::{error, info},
};

pub fn connect_proxy<T: fidl::endpoints::ProtocolMarker>(path: &str) -> Result<T::Proxy> {
    let (proxy, server) = fidl::endpoints::create_proxy::<T>()
        .map_err(|e| format_err!("Failed to create proxy: {}", e))?;

    fdio::service_connect(path, server.into_channel())
        .map_err(|s| format_err!("Failed to connect to service at {}: {}", path, s))?;
    Ok(proxy)
}

pub async fn get_driver_topological_path(path: &str) -> Result<String> {
    let controller_path = path.to_owned() + "/device_controller";
    let proxy = connect_proxy::<fdevice::ControllerMarker>(&controller_path)?;
    proxy
        .get_topological_path()
        .await?
        .map_err(|raw| format_err!("zx error: {}", zx::Status::from_raw(raw)))
}

pub async fn list_drivers(path: &str) -> Vec<String> {
    let dir = match fuchsia_fs::directory::open_in_namespace(
        path,
        fuchsia_fs::OpenFlags::RIGHT_READABLE,
    ) {
        Ok(s) => s,
        Err(err) => {
            info!(%path, %err, "Service directory doesn't exist or NodeProxy failed with error");
            return Vec::new();
        }
    };
    match fuchsia_fs::directory::readdir(&dir).await {
        Ok(s) => s.iter().map(|dir_entry| dir_entry.name.clone()).collect(),
        Err(err) => {
            error!(%path, %err, "Read service directory failed with error");
            Vec::new()
        }
    }
}

// Representation of an actively-used driver.
pub struct Driver<T> {
    pub name: String,
    pub proxy: T,
}
