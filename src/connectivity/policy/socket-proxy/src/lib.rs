// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of the network socket proxy.
//!
//! Runs proxied versions of fuchsia.posix.socket.Provider and fuchsia.posix.socket.raw.Provider.
//! Exposes fuchsia.net.policy.socketproxy.StarnixNetworks,
//! fuchsia.net.policy.socketproxy.FuchsiaNetworks, and
//! fuchsia.net.policy.socketproxy.DnsServerWatcher.

use anyhow::Context as _;
use fidl_fuchsia_posix_socket::{self as fposix_socket, OptionalUint32};
use fuchsia_component::server::{ServiceFs, ServiceFsDir};
use fuchsia_inspect::health::Reporter;
use fuchsia_inspect_derive::{Inspect, WithInspect as _};
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::StreamExt as _;
use log::error;
use std::sync::Arc;
use {fidl_fuchsia_net as fnet, fidl_fuchsia_net_policy_socketproxy as fnp_socketproxy};

mod dns_watcher;
mod registry;
mod socket_provider;

pub use registry::{NetworkConversionError, NetworkExt, NetworkRegistryError};

#[derive(Copy, Clone, Debug)]
struct SocketMarks {
    mark_1: OptionalUint32,
    mark_2: OptionalUint32,
}

impl From<SocketMarks> for fnet::Marks {
    fn from(SocketMarks { mark_1, mark_2 }: SocketMarks) -> Self {
        let into_option_u32 = |opt| match opt {
            OptionalUint32::Unset(fposix_socket::Empty) => None,
            OptionalUint32::Value(val) => Some(val),
        };
        Self {
            mark_1: into_option_u32(mark_1),
            mark_2: into_option_u32(mark_2),
            __source_breaking: fidl::marker::SourceBreaking,
        }
    }
}

impl SocketMarks {
    fn has_value(&self) -> bool {
        match (self.mark_1, self.mark_2) {
            (OptionalUint32::Value(_), _) => true,
            (_, OptionalUint32::Value(_)) => true,
            _ => false,
        }
    }
}

impl Default for SocketMarks {
    fn default() -> Self {
        Self {
            mark_1: OptionalUint32::Unset(fposix_socket::Empty),
            mark_2: OptionalUint32::Unset(fposix_socket::Empty),
        }
    }
}

#[derive(Inspect)]
struct SocketProxy {
    registry: registry::Registry,
    dns_watcher: dns_watcher::DnsServerWatcher,
    socket_provider: socket_provider::SocketProvider,
}

impl SocketProxy {
    fn new() -> Result<Self, anyhow::Error> {
        let mark = Arc::new(Mutex::new(SocketMarks::default()));
        let (dns_tx, dns_rx) = mpsc::channel(1);
        Ok(Self {
            registry: registry::Registry::new(mark.clone(), dns_tx)
                .context("while creating registry")?,
            dns_watcher: dns_watcher::DnsServerWatcher::new(Arc::new(Mutex::new(dns_rx))),
            socket_provider: socket_provider::SocketProvider::new(mark),
        })
    }
}

enum IncomingService {
    StarnixNetworks(fnp_socketproxy::StarnixNetworksRequestStream),
    FuchsiaNetworks(fnp_socketproxy::FuchsiaNetworksRequestStream),
    DnsServerWatcher(fnp_socketproxy::DnsServerWatcherRequestStream),
    PosixSocket(fidl_fuchsia_posix_socket::ProviderRequestStream),
    PosixSocketRaw(fidl_fuchsia_posix_socket_raw::ProviderRequestStream),
}

enum BackgroundTask {
    UpdateDefaultNetworks,
}

/// Main entry point for the network socket proxy.
pub async fn run() -> Result<(), anyhow::Error> {
    fuchsia_inspect::component::health().set_starting_up();

    let inspector = fuchsia_inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());

    let proxy = SocketProxy::new()?.with_inspect(inspector.root(), "root")?;

    let mut fs = ServiceFs::new_local();
    let _: &mut ServiceFsDir<'_, _> = fs
        .dir("svc")
        .add_fidl_service(IncomingService::StarnixNetworks)
        .add_fidl_service(IncomingService::FuchsiaNetworks)
        .add_fidl_service(IncomingService::DnsServerWatcher)
        .add_fidl_service(IncomingService::PosixSocket)
        .add_fidl_service(IncomingService::PosixSocketRaw);

    let _: &mut ServiceFs<_> = fs.take_and_serve_directory_handle()?;

    fuchsia_inspect::component::health().set_ok();

    let (task_tx, task_rx) = mpsc::channel(1);

    let fidl = fs.for_each_concurrent(100, |service| {
        let task_tx = task_tx.clone();
        let proxy = &proxy;
        async move {
            match service {
                IncomingService::StarnixNetworks(stream) => {
                    proxy.registry.run_starnix(stream, task_tx).await
                }
                IncomingService::FuchsiaNetworks(stream) => {
                    proxy.registry.run_fuchsia(stream, task_tx).await
                }
                IncomingService::DnsServerWatcher(stream) => proxy.dns_watcher.run(stream).await,
                IncomingService::PosixSocket(stream) => proxy.socket_provider.run(stream).await,
                IncomingService::PosixSocketRaw(stream) => {
                    proxy.socket_provider.run_raw(stream).await
                }
            }
            .unwrap_or_else(|e| error!("{e:?}"))
        }
    });

    let background_tasks = task_rx.for_each(|task| async {
        match task {
            BackgroundTask::UpdateDefaultNetworks => proxy.registry.update_default_network().await,
        }
        .unwrap_or_else(|e| error!("{e:?}"))
    });

    futures::future::join(fidl, background_tasks).await;

    Ok(())
}
