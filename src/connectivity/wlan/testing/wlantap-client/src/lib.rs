// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error, fidl::endpoints::Proxy, fidl_fuchsia_wlan_tap as wlantap, fuchsia_zircon as zx,
};

pub struct Wlantap {
    proxy: wlantap::WlantapCtlProxy,
}

impl Wlantap {
    pub async fn open() -> Result<Self, Error> {
        let dir = fuchsia_fs::directory::open_in_namespace(
            "/dev",
            fuchsia_fs::OpenFlags::RIGHT_READABLE,
        )?;
        let proxy =
            device_watcher::recursive_wait_and_open_node(&dir, "sys/test/wlantapctl").await?;
        let proxy = proxy.into_channel().expect("Proxy::into_channel");
        let proxy = Proxy::from_channel(proxy);
        Ok(Self { proxy })
    }

    pub async fn create_phy(
        &self,
        mut config: wlantap::WlantapPhyConfig,
    ) -> Result<wlantap::WlantapPhyProxy, Error> {
        let Self { proxy } = self;
        let (ours, theirs) = fidl::endpoints::create_proxy()?;

        let status = proxy.create_phy(&mut config, theirs).await?;
        let () = zx::ok(status)?;

        Ok(ours)
    }
}
