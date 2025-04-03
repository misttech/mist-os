// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::helpers::rediscover_helper;
use anyhow::{Context as _, Result};
use async_trait::async_trait;
use discovery::{FastbootConnectionState, TargetFilter, TargetHandle, TargetState};
use ffx_fastboot_interface::interface_factory::{
    InterfaceFactory, InterfaceFactoryBase, InterfaceFactoryError,
};
use ffx_fastboot_transport_interface::udp::{open, UdpNetworkInterface};
use fuchsia_async::Timer;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

///////////////////////////////////////////////////////////////////////////////
// UdpFactory
//

#[derive(Debug, Clone)]
pub struct UdpFactory {
    target_name: String,
    fastboot_devices_file_path: Option<PathBuf>,
    addr: SocketAddr,
    open_retries: u64,
    retry_wait_seconds: u64,
}

impl UdpFactory {
    pub fn new(
        target_name: String,
        fastboot_devices_file_path: Option<PathBuf>,
        addr: SocketAddr,
        open_retries: u64,
        retry_wait_seconds: u64,
    ) -> Self {
        Self { target_name, fastboot_devices_file_path, addr, open_retries, retry_wait_seconds }
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
    async fn open(&mut self) -> Result<UdpNetworkInterface, InterfaceFactoryError> {
        let wait_duration = Duration::from_secs(self.retry_wait_seconds);
        for i in 1..self.open_retries {
            match open(self.addr)
                .await
                .with_context(|| format!("connecting via UDP to Fastboot address: {}", self.addr))
            {
                Ok(interface) => return Ok(interface),

                Err(e) => {
                    tracing::debug!(
                        "Attempt {}. Got error connecting to fastboot address:{}",
                        i,
                        e,
                    );

                    Timer::new(wait_duration).await;
                }
            }
        }
        Err(InterfaceFactoryError::ConnectionError("UDP".to_string(), self.addr, self.open_retries))
    }

    async fn close(&self) {
        tracing::debug!("Closing Fastboot UDP Factory for: {}", self.addr);
    }

    async fn rediscover(&mut self) -> Result<(), InterfaceFactoryError> {
        let filter = UdpTargetFilter { node_name: self.target_name.clone() };

        rediscover_helper(
            &self.fastboot_devices_file_path,
            &self.target_name,
            filter,
            &mut |connection_state| {
                match connection_state {
                    FastbootConnectionState::Udp(addrs) => {
                        self.addr = addrs.iter().find_map(|x| x.try_into().ok()).unwrap();
                    }
                    s @ _ => {
                        return Err(InterfaceFactoryError::RediscoverTargetNotInCorrectTransport(
                            self.target_name.clone(),
                            "UDP".to_string(),
                            s.to_string(),
                        ))
                    }
                }
                Ok(())
            },
        )
        .await
    }
}

impl InterfaceFactory<UdpNetworkInterface> for UdpFactory {}

pub struct UdpTargetFilter {
    node_name: String,
}

impl TargetFilter for UdpTargetFilter {
    fn filter_target(&mut self, handle: &TargetHandle) -> bool {
        if handle.node_name.as_ref() != Some(&self.node_name) {
            return false;
        }
        match &handle.state {
            TargetState::Fastboot(ts)
                if matches!(ts.connection_state, FastbootConnectionState::Udp(_)) =>
            {
                tracing::trace!("Filtered and found target handle: {}", handle);
                true
            }
            state @ _ => {
                tracing::debug!("Target state {} is not  UDP Fastboot... skipping", state);
                false
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use addr::TargetIpAddr;
    use std::net::{IpAddr, Ipv4Addr};

    ///////////////////////////////////////////////////////////////////////////////
    // UdpTargetFilter
    //

    #[test]
    fn filter_target_test() -> Result<()> {
        let node_name = "jod".to_string();
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let addr = TargetIpAddr::from(socket);

        let mut filter = UdpTargetFilter { node_name };

        // Passes
        assert!(filter.filter_target(&TargetHandle {
            node_name: Some("jod".to_string()),
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "".to_string(),
                connection_state: FastbootConnectionState::Udp(vec![addr])
            })
        }));
        // Fails: wrong name
        assert!(!filter.filter_target(&TargetHandle {
            node_name: Some("Wake".to_string()),
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "".to_string(),
                connection_state: FastbootConnectionState::Udp(vec![addr])
            })
        }));
        // Fails: wrong state
        assert!(!filter.filter_target(&TargetHandle {
            node_name: Some("jod".to_string()),
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "".to_string(),
                connection_state: FastbootConnectionState::Tcp(vec![addr])
            })
        }));
        // Fails: Bad name
        assert!(!filter.filter_target(&TargetHandle {
            node_name: None,
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "".to_string(),
                connection_state: FastbootConnectionState::Udp(vec![addr])
            })
        }));
        Ok(())
    }
}
