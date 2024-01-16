// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use discovery::{
    wait_for_devices, DiscoverySources, FastbootConnectionState, FastbootTargetState, TargetFilter,
    TargetHandle, TargetState,
};
use ffx_fastboot_interface::interface_factory::{InterfaceFactory, InterfaceFactoryBase};
use futures::StreamExt;
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

struct UsbTargetFilter {
    target_serial: String,
}

impl TargetFilter for UsbTargetFilter {
    fn filter_target(&mut self, handle: &TargetHandle) -> bool {
        handle.state
            == TargetState::Fastboot(FastbootTargetState {
                serial_number: self.target_serial.clone(),
                connection_state: FastbootConnectionState::Usb(self.target_serial.clone()),
            })
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

    async fn rediscover(&mut self) -> Result<()> {
        tracing::debug!("Rediscovering devices");
        let filter = UsbTargetFilter { target_serial: self.serial.clone() };
        // Only want added events
        let mut device_stream = wait_for_devices(filter, true, false, DiscoverySources::USB)?;
        while let Some(s) = device_stream.next().await {
            if s.is_ok() {
                return Ok(());
            }
        }
        tracing::error!("We shouldnt have gotten here");
        Ok(())
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

#[cfg(test)]
mod test {
    use super::*;
    use addr::TargetAddr;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    ///////////////////////////////////////////////////////////////////////////////
    // UsbTargetFilter
    //

    #[test]
    fn filter_target_test() -> Result<()> {
        let target_serial = "1234567890".to_string();

        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let addr = TargetAddr::from(socket);

        let mut filter = UsbTargetFilter { target_serial };

        // Passes
        assert!(filter.filter_target(&TargetHandle {
            node_name: None,
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "1234567890".to_string(),
                connection_state: FastbootConnectionState::Usb("1234567890".to_string())
            })
        }));
        // Fails: wrong serial
        assert!(!filter.filter_target(&TargetHandle {
            node_name: None,
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "ABCD".to_string(),
                connection_state: FastbootConnectionState::Usb("1234567890".to_string())
            })
        }));
        // Fails: wrong connection_state serial
        assert!(!filter.filter_target(&TargetHandle {
            node_name: None,
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "1234567890".to_string(),
                connection_state: FastbootConnectionState::Usb("ABCD".to_string())
            })
        }));
        // Fails: Uses Udp
        assert!(!filter.filter_target(&TargetHandle {
            node_name: Some("Kiriona".to_string()),
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "1234567890".to_string(),
                connection_state: FastbootConnectionState::Udp(addr)
            })
        }));
        // Fails: Uses Tcp
        assert!(!filter.filter_target(&TargetHandle {
            node_name: Some("Kiriona".to_string()),
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "1234567890".to_string(),
                connection_state: FastbootConnectionState::Tcp(addr)
            })
        }));
        Ok(())
    }
}
