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
use netext::TokioAsyncWrapper;
use std::net::SocketAddr;
use std::path::PathBuf;
use thiserror::Error;
use tokio::net::TcpStream;
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

const UDP_RETRY_COUNT: &str = "fastboot.network.udp.retry_count";
const UDP_RETRY_COUNT_DEFAULT: u64 = 3;
const UDP_WAIT_SECONDS: &str = "fastboot.network.udp.retry_wait_seconds";
const UDP_WAIT_SECONDS_DEFAULT: u64 = 2;
const TCP_RETRY_COUNT: &str = "fastboot.network.tcp.retry_count";
const TCP_RETRY_COUNT_DEFAULT: u64 = 3;
const TCP_WAIT_SECONDS: &str = "fastboot.network.udp.retry_wait_seconds";
const TCP_WAIT_SECONDS_DEFAULT: u64 = 2;

pub struct FastbootNetworkConnectionConfig {
    retry_wait_seconds: u64,
    retry_count: u64,
}

impl FastbootNetworkConnectionConfig {
    pub fn new(retry_wait_seconds: u64, retry_count: u64) -> Self {
        Self { retry_wait_seconds, retry_count }
    }

    async fn new_from_config(
        retry_key: &str,
        retry_default: u64,
        wait_key: &str,
        wait_default: u64,
    ) -> Self {
        let retry_count = ffx_config::get(retry_key).unwrap_or(retry_default);
        let retry_wait_seconds = ffx_config::get(wait_key).unwrap_or(wait_default);
        Self::new(retry_wait_seconds, retry_count)
    }

    pub async fn new_tcp() -> Self {
        Self::new_from_config(
            TCP_RETRY_COUNT,
            TCP_RETRY_COUNT_DEFAULT,
            TCP_WAIT_SECONDS,
            TCP_WAIT_SECONDS_DEFAULT,
        )
        .await
    }

    pub async fn new_udp() -> Self {
        Self::new_from_config(
            UDP_RETRY_COUNT,
            UDP_RETRY_COUNT_DEFAULT,
            UDP_WAIT_SECONDS,
            UDP_WAIT_SECONDS_DEFAULT,
        )
        .await
    }
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
                let config = FastbootNetworkConnectionConfig::new_tcp().await;
                let fastboot_device_file_path: Option<PathBuf> =
                    ffx_config::get(fastboot_file_discovery::FASTBOOT_FILE_PATH).ok();
                Ok(Box::new(
                    tcp_proxy(target_name, fastboot_device_file_path, &addr, config).await?,
                ))
            }
            FastbootConnectionKind::Udp(target_name, addr) => {
                let config = FastbootNetworkConnectionConfig::new_udp().await;
                let fastboot_device_file_path: Option<PathBuf> =
                    ffx_config::get(fastboot_file_discovery::FASTBOOT_FILE_PATH).ok();
                Ok(Box::new(
                    udp_proxy(target_name, fastboot_device_file_path, &addr, config).await?,
                ))
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

/// Creates a FastbootProxy over TCP for a device at the given SocketAddr
pub async fn tcp_proxy(
    target_name: String,
    fastboot_device_file_path: Option<PathBuf>,
    addr: &SocketAddr,
    config: FastbootNetworkConnectionConfig,
) -> Result<FastbootProxy<TcpNetworkInterface<TokioAsyncWrapper<TcpStream>>>> {
    if target_name.is_empty() {
        bail!(FastbootConnectionFactoryError::EmptyTargetName);
    }

    let mut factory = TcpFactory::new(
        target_name,
        fastboot_device_file_path,
        *addr,
        config.retry_count,
        config.retry_wait_seconds,
    );
    let interface = factory
        .open()
        .await
        .with_context(|| format!("FastbootProxy connecting via TCP to Fastboot address: {addr}"))?;
    Ok(FastbootProxy::<TcpNetworkInterface<TokioAsyncWrapper<TcpStream>>>::new(
        addr.to_string(),
        interface,
        factory,
    ))
}

///////////////////////////////////////////////////////////////////////////////
// UdpInterface
//

/// Creates a FastbootProxy over TCP for a device at the given SocketAddr
pub async fn udp_proxy(
    target_name: String,
    fastboot_device_file_path: Option<PathBuf>,
    addr: &SocketAddr,
    config: FastbootNetworkConnectionConfig,
) -> Result<FastbootProxy<UdpNetworkInterface>> {
    if target_name.is_empty() {
        bail!(FastbootConnectionFactoryError::EmptyTargetName);
    }

    let mut factory = UdpFactory::new(
        target_name,
        fastboot_device_file_path,
        *addr,
        config.retry_count,
        config.retry_wait_seconds,
    );
    let interface = factory
        .open()
        .await
        .with_context(|| format!("connecting via UDP to Fastboot address: {addr}"))?;
    Ok(FastbootProxy::<UdpNetworkInterface>::new(addr.to_string(), interface, factory))
}
