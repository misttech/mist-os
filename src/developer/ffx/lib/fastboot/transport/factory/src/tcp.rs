// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, bail, Context as _, Result};
use async_trait::async_trait;
use discovery::{
    wait_for_devices, DiscoverySources, FastbootConnectionState, TargetEvent, TargetFilter,
    TargetHandle, TargetState,
};
use ffx_fastboot_interface::interface_factory::{InterfaceFactory, InterfaceFactoryBase};
use ffx_fastboot_transport_interface::tcp::{open_once, TcpNetworkInterface};
use fuchsia_async::Timer;
use futures::StreamExt;
use std::net::SocketAddr;
use std::time::Duration;

///////////////////////////////////////////////////////////////////////////////
// TcpFactory
//

#[derive(Debug, Clone)]
pub struct TcpFactory {
    target_name: String,
    addr: SocketAddr,
    open_retries: u64,
    retry_wait_seconds: u64,
}

impl TcpFactory {
    pub fn new(
        target_name: String,
        addr: SocketAddr,
        open_retries: u64,
        retry_wait_seconds: u64,
    ) -> Self {
        Self { target_name, addr, open_retries, retry_wait_seconds }
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
        let wait_duration = Duration::from_secs(self.retry_wait_seconds);
        for i in 1..self.open_retries {
            match open_once(&self.addr, Duration::from_secs(1)).await.with_context(|| {
                format!("TCPFactory connecting via TCP to Fastboot address: {}", self.addr)
            }) {
                Err(e) => {
                    tracing::debug!(
                        "Attempt {}. Got error connecting to fastboot address:{}",
                        i,
                        e,
                    );

                    Timer::new(wait_duration).await;
                }
                Ok(interface) => return Ok(interface),
            }
        }
        Err(anyhow!(
            "Could not connect via TCP to fastboot address: {} after {} tries",
            self.addr,
            self.open_retries
        ))
    }

    async fn close(&self) {
        tracing::debug!("Closing Fastboot TCP Factory for: {}", self.addr);
    }

    async fn rediscover(&mut self) -> Result<()> {
        let filter = TcpTargetFilter { node_name: self.target_name.clone() };
        // Only want added events
        let mut device_stream = wait_for_devices(
            filter,
            true,
            false,
            DiscoverySources::MDNS | DiscoverySources::MANUAL,
        )?;

        if let Some(s) = device_stream.next().await {
            if let Ok(event) = s {
                // This is the first event that matches our filter.
                // Mutate our internal understanding of the address
                // the target is at with the new address discovered
                match event {
                    TargetEvent::Removed(_) => {
                        bail!("When rediscovering target: {}, expected a target Added event but got a Removed event", self.target_name.clone())
                    }
                    TargetEvent::Added(handle) => match handle.state {
                        TargetState::Fastboot(ts) => match ts.connection_state {
                            FastbootConnectionState::Tcp(addr) => self.addr = addr.into(),
                            _ => bail!("When rediscovering target: {}, expected target to reconnect in TCP mode", self.target_name.clone()),
                        },
                         state @ _ => bail!("When rediscovering target: {}, expected target to be rediscovered in Fastboot mode. Got: {}", self.target_name.clone(), state),
                    },
                }
                return Ok(());
            }
        }
        Ok(())
    }
}

impl InterfaceFactory<TcpNetworkInterface> for TcpFactory {}

pub struct TcpTargetFilter {
    node_name: String,
}

impl TargetFilter for TcpTargetFilter {
    fn filter_target(&mut self, handle: &TargetHandle) -> bool {
        if handle.node_name != Some(self.node_name.clone()) {
            tracing::debug!(
                "Discovered target name \"{:#?}\" does not match desired \"{}\"... skipping",
                handle.node_name,
                self.node_name.clone()
            );
            return false;
        }
        match &handle.state {
            TargetState::Fastboot(ts)
                if matches!(ts.connection_state, FastbootConnectionState::Tcp(_)) =>
            {
                tracing::debug!("Filtered and found target handle: {}", handle);
                true
            }
            state @ _ => {
                tracing::debug!("Target state {} is not  TCP Fastboot... skipping", state);
                false
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use addr::TargetAddr;
    use std::net::{IpAddr, Ipv4Addr};

    ///////////////////////////////////////////////////////////////////////////////
    // TcpTargetFilter
    //

    #[test]
    fn filter_target_test() -> Result<()> {
        let node_name = "jod".to_string();
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let addr = TargetAddr::from(socket);

        let mut filter = TcpTargetFilter { node_name };

        // Passes
        assert!(filter.filter_target(&TargetHandle {
            node_name: Some("jod".to_string()),
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "".to_string(),
                connection_state: FastbootConnectionState::Tcp(addr)
            })
        }));
        // Fails: wrong name
        assert!(!filter.filter_target(&TargetHandle {
            node_name: Some("Wake".to_string()),
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "".to_string(),
                connection_state: FastbootConnectionState::Tcp(addr)
            })
        }));
        // Fails: wrong state
        assert!(!filter.filter_target(&TargetHandle {
            node_name: Some("jod".to_string()),
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "".to_string(),
                connection_state: FastbootConnectionState::Udp(addr)
            })
        }));
        // Fails: Bad name
        assert!(!filter.filter_target(&TargetHandle {
            node_name: None,
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "".to_string(),
                connection_state: FastbootConnectionState::Tcp(addr)
            })
        }));
        Ok(())
    }
}
