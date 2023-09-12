// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(unused_imports, unused_variables, dead_code)]
use crate::{fastboot::InterfaceFactory, target::Target};
use anyhow::{anyhow, bail, Context as _, Result};
use async_net::TcpStream;
use async_trait::async_trait;
use ffx_config::get;
use ffx_fastboot::transport::tcp::{
    open_once, TcpNetworkInterface, HANDSHAKE_TIMEOUT, HANDSHAKE_TIMEOUT_MILLIS, RETRY_WAIT_SECONDS,
};
use futures::{
    prelude::*,
    task::{Context, Poll},
};
use std::time::Duration;
use std::{convert::TryInto, io::ErrorKind, net::SocketAddr, pin::Pin};
use timeout::timeout;
use tracing::debug;

const OPEN_RETRY: u64 = 10;
/// Number of times to retry when connecting to a target in fastboot over TCP
const OPEN_RETRY_COUNT: &str = "fastboot.tcp.open.retry.count";
/// Time to wait for a response when connecting to a target in fastboot over TCP
const OPEN_RETRY_WAIT: &str = "fastboot.tcp.open.retry.wait";

pub struct TcpNetworkFactory {}

impl TcpNetworkFactory {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn open_with_retry(
        &mut self,
        target: &Target,
        retry_count: u64,
        retry_wait_seconds: u64,
    ) -> Result<TcpNetworkInterface> {
        let handshake_timeout_millis =
            get(HANDSHAKE_TIMEOUT).await.unwrap_or(HANDSHAKE_TIMEOUT_MILLIS);
        for retry in 0..retry_count {
            let addr: SocketAddr = target
                .fastboot_address()
                .ok_or(anyhow!("No network address for fastboot"))?
                .0
                .into();
            match open_once(&addr, Duration::from_millis(handshake_timeout_millis)).await {
                Ok(res) => {
                    tracing::debug!("TCP connect attempt #{} succeeds", retry);
                    return Ok(res);
                }
                Err(e) => {
                    if retry + 1 < retry_count {
                        tracing::debug!("TCP connect attempt #{} failed", retry);
                        std::thread::sleep(std::time::Duration::from_secs(retry_wait_seconds));
                        continue;
                    }

                    return Err(e);
                }
            }
        }

        Err(anyhow::format_err!("Unreachable"))
    }
}

#[async_trait(?Send)]
impl InterfaceFactory<TcpNetworkInterface> for TcpNetworkFactory {
    async fn open(&mut self, target: &Target) -> Result<TcpNetworkInterface> {
        let retry_count: u64 = get(OPEN_RETRY_COUNT).await.unwrap_or(OPEN_RETRY);
        let retry_wait_seconds: u64 = get(OPEN_RETRY_WAIT).await.unwrap_or(RETRY_WAIT_SECONDS);

        self.open_with_retry(target, retry_count, retry_wait_seconds).await
    }

    async fn close(&self) {}

    async fn is_target_discovery_enabled(&self) -> bool {
        ffx_config::get("discovery.mdns.enabled").await.unwrap_or(true)
    }
}
