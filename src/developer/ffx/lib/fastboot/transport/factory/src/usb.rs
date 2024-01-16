// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use ffx_fastboot_interface::interface_factory::{InterfaceFactory, InterfaceFactoryBase};
use usb_bulk::AsyncInterface;
use usb_fastboot_discovery::open_interface_with_serial;

///////////////////////////////////////////////////////////////////////////////
// UsbFactory
//

#[derive(Default, Debug, Clone)]
pub struct UsbFactory {
    serial: String,
}

impl UsbFactory {
    pub fn new(serial: String) -> Self {
        Self { serial }
    }
}

#[async_trait(?Send)]
impl InterfaceFactoryBase<AsyncInterface> for UsbFactory {
    async fn open(&mut self) -> Result<AsyncInterface> {
        let interface = open_interface_with_serial(&self.serial).await.with_context(|| {
            format!("Failed to open target usb interface by serial {}", self.serial)
        })?;
        tracing::debug!("serial now in use: {}", self.serial);
        Ok(interface)
    }

    async fn close(&self) {
        tracing::debug!("dropping in use serial: {}", self.serial);
    }
}

impl Drop for UsbFactory {
    fn drop(&mut self) {
        futures::executor::block_on(async move {
            self.close().await;
        });
    }
}

impl InterfaceFactory<AsyncInterface> for UsbFactory {}
