// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use {fidl_fuchsia_io as fio, fidl_fuchsia_wlan_tap as wlantap};

pub struct Wlantap {
    proxy: wlantap::WlantapCtlProxy,
}

impl Wlantap {
    pub async fn open() -> Result<Self, Error> {
        let dir = fuchsia_fs::directory::open_in_namespace("/dev", fuchsia_fs::Flags::empty())?;
        Self::open_from_devfs(&dir).await
    }

    pub async fn open_from_devfs(devfs: &fio::DirectoryProxy) -> Result<Self, Error> {
        let proxy = device_watcher::recursive_wait_and_open::<wlantap::WlantapCtlMarker>(
            devfs,
            "sys/test/wlantapctl",
        )
        .await?;
        Ok(Self { proxy })
    }

    pub async fn create_phy(
        &self,
        config: wlantap::WlantapPhyConfig,
    ) -> Result<wlantap::WlantapPhyProxy, Error> {
        let Self { proxy } = self;
        let (ours, theirs) = fidl::endpoints::create_proxy();

        let status = proxy.create_phy(&config, theirs).await?;
        let () = zx::ok(status)?;

        Ok(ours)
    }
}
