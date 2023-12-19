// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use ffx_fastboot_interface::interface_factory::{InterfaceFactory, InterfaceFactoryBase};
use ffx_fastboot_transport_interface::tcp::{open_once, TcpNetworkInterface};
use std::net::SocketAddr;
use std::time::Duration;

///////////////////////////////////////////////////////////////////////////////
// TcpFactory
//

#[derive(Debug, Clone)]
pub struct TcpFactory {
    addr: SocketAddr,
}

impl TcpFactory {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
    }
}

impl Drop for TcpFactory {
    fn drop(&mut self) {
        futures::executor::block_on(async move {
            self.close().await;
        });
    }
}

#[async_trait(?Send)]
impl InterfaceFactoryBase<TcpNetworkInterface> for TcpFactory {
    async fn open(&mut self) -> Result<TcpNetworkInterface> {
        let interface = open_once(&self.addr, Duration::from_secs(1))
            .await
            .with_context(|| format!("connecting via TCP to Fastboot address: {}", self.addr))?;
        Ok(interface)
    }

    async fn close(&self) {
        tracing::debug!("Closing Fastboot TCP Factory for: {}", self.addr);
    }
}

impl InterfaceFactory<TcpNetworkInterface> for TcpFactory {}
