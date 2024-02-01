// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use ffx_fastboot_interface::interface_factory::{InterfaceFactory, InterfaceFactoryBase};
use ffx_fastboot_transport_interface::udp::{open, UdpNetworkInterface};
use std::net::SocketAddr;

///////////////////////////////////////////////////////////////////////////////
// UdpFactory
//

#[derive(Debug, Clone)]
pub struct UdpFactory {
    addr: SocketAddr,
}

impl UdpFactory {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
    }
}

impl Drop for UdpFactory {
    fn drop(&mut self) {
        futures::executor::block_on(async move {
            self.close().await;
        });
    }
}

#[async_trait(?Send)]
impl InterfaceFactoryBase<UdpNetworkInterface> for UdpFactory {
    async fn open(&mut self) -> Result<UdpNetworkInterface> {
        let interface = open(self.addr)
            .await
            .with_context(|| format!("connecting via UDP to Fastboot address: {}", self.addr))?;
        Ok(interface)
    }

    async fn close(&self) {
        tracing::debug!("Closing Fastboot UDP Factory for: {}", self.addr);
    }
}

impl InterfaceFactory<UdpNetworkInterface> for UdpFactory {}
