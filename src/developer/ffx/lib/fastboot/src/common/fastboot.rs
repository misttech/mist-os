// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use ffx_fastboot_interface::fastboot_interface::FastbootInterface;
use ffx_fastboot_interface::fastboot_proxy::FastbootProxy;
use ffx_fastboot_interface::interface_factory::InterfaceFactoryBase;
use ffx_fastboot_transport_factory::tcp::TcpFactory;
use ffx_fastboot_transport_factory::udp::UdpFactory;
use ffx_fastboot_transport_factory::usb::UsbFactory;
use ffx_fastboot_transport_interface::tcp::TcpNetworkInterface;
use ffx_fastboot_transport_interface::udp::UdpNetworkInterface;
use std::net::SocketAddr;
use thiserror::Error;
use usb_bulk::AsyncInterface;

#[derive(Error, Debug, Clone)]
enum FastbootConnectionFactoryError {
    #[error("Passed Target Name was empty. Target name needs to be a non-empty string to support Target rediscovery")]
    EmptyTargetName,
}

///////////////////////////////////////////////////////////////////////////////
// ConnectionFactory
//

pub enum FastbootConnectionKind {
    Usb(String),
    Tcp(String, SocketAddr),
    Udp(String, SocketAddr),
}

#[async_trait(?Send)]
pub trait FastbootConnectionFactory {
    async fn build_interface(
        &self,
        connection: FastbootConnectionKind,
    ) -> Result<Box<dyn FastbootInterface>>;
}

pub struct ConnectionFactory {}

#[async_trait(?Send)]
impl FastbootConnectionFactory for ConnectionFactory {
    async fn build_interface(
        &self,
        connection: FastbootConnectionKind,
    ) -> Result<Box<dyn FastbootInterface>> {
        match connection {
            FastbootConnectionKind::Usb(serial_number) => {
                Ok(Box::new(usb_proxy(serial_number).await?))
            }
            FastbootConnectionKind::Tcp(target_name, addr) => {
                Ok(Box::new(tcp_proxy(target_name, &addr).await?))
            }
            FastbootConnectionKind::Udp(target_name, addr) => {
                Ok(Box::new(udp_proxy(target_name, &addr).await?))
            }
        }
    }
}

///////////////////////////////////////////////////////////////////////////////
// AsyncInterface
//

/// Creates a FastbootProxy over USB for a device with the given serial number
pub async fn usb_proxy(serial_number: String) -> Result<FastbootProxy<AsyncInterface>> {
    let mut interface_factory = UsbFactory::new(serial_number.clone());
    let interface = interface_factory.open().await.with_context(|| {
        format!("Failed to open target usb interface by serial {serial_number}")
    })?;

    Ok(FastbootProxy::<AsyncInterface>::new(serial_number, interface, interface_factory))
}

///////////////////////////////////////////////////////////////////////////////
// TcpInterface
//

const TCP_N_OPEN_RETRIES: u64 = 5;
const TCP_RETRY_WAIT_SECONDS: u64 = 1;

/// Creates a FastbootProxy over TCP for a device at the given SocketAddr
pub async fn tcp_proxy(
    target_name: String,
    addr: &SocketAddr,
) -> Result<FastbootProxy<TcpNetworkInterface>> {
    if target_name.is_empty() {
        bail!(FastbootConnectionFactoryError::EmptyTargetName);
    }

    let mut factory =
        TcpFactory::new(target_name, *addr, TCP_N_OPEN_RETRIES, TCP_RETRY_WAIT_SECONDS);
    let interface = factory
        .open()
        .await
        .with_context(|| format!("FastbootProxy connecting via TCP to Fastboot address: {addr}"))?;
    Ok(FastbootProxy::<TcpNetworkInterface>::new(addr.to_string(), interface, factory))
}

///////////////////////////////////////////////////////////////////////////////
// UdpInterface
//

const UDP_N_OPEN_RETRIES: u64 = 5;
const UDP_RETRY_WAIT_SECONDS: u64 = 1;

/// Creates a FastbootProxy over TCP for a device at the given SocketAddr
pub async fn udp_proxy(
    target_name: String,
    addr: &SocketAddr,
) -> Result<FastbootProxy<UdpNetworkInterface>> {
    if target_name.is_empty() {
        bail!(FastbootConnectionFactoryError::EmptyTargetName);
    }

    let mut factory =
        UdpFactory::new(target_name, *addr, UDP_N_OPEN_RETRIES, UDP_RETRY_WAIT_SECONDS);
    let interface = factory
        .open()
        .await
        .with_context(|| format!("connecting via UDP to Fastboot address: {addr}"))?;
    Ok(FastbootProxy::<UdpNetworkInterface>::new(addr.to_string(), interface, factory))
}
