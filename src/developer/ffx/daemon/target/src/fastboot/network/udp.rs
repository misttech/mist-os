// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{fastboot::InterfaceFactory, target::Target};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use ffx_fastboot::transport::udp::open as udp_open;
use ffx_fastboot::transport::udp::UdpNetworkInterface;
use std::net::SocketAddr;

pub struct UdpNetworkFactory {}

impl UdpNetworkFactory {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait(?Send)]
impl InterfaceFactory<UdpNetworkInterface> for UdpNetworkFactory {
    async fn open(&mut self, target: &Target) -> Result<UdpNetworkInterface> {
        let addr = target.fastboot_address().ok_or(anyhow!("No network address for fastboot"))?.0;
        let to_sock: SocketAddr = addr.into();
        udp_open(to_sock).await
    }

    async fn close(&self) {}

    async fn is_target_discovery_enabled(&self) -> bool {
        ffx_config::get("discovery.mdns.enabled").await.unwrap_or(true)
    }
}
