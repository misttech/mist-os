// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of the network socket proxy.
//!
//! Runs proxied versions of fuchsia.posix.socket.Provider and fuchsia.posix.socket.raw.Provider.
//! Exposes fuchsia.netpol.socketproxy.StarnixNetworks and
//! fuchsia.netpol.socketproxy.DnsServerWatcher.

use fidl_fuchsia_posix_socket::{self as fposix_socket, OptionalUint32};
use fuchsia_component::server::{ServiceFs, ServiceFsDir};
use fuchsia_inspect::health::Reporter;
use fuchsia_inspect_derive::{Inspect, WithInspect as _};
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::StreamExt as _;
use std::sync::Arc;
use tracing::error;

mod dns_watcher;
mod registry;
mod socket_provider;

#[derive(Copy, Clone, Debug)]
struct SocketMarks {
    mark_1: OptionalUint32,
    mark_2: OptionalUint32,
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
    fn new() -> Self {
        let mark = Arc::new(Mutex::new(SocketMarks::default()));
        let (dns_tx, dns_rx) = mpsc::channel(1);
        Self {
            registry: registry::Registry::new(mark.clone(), dns_tx),
            dns_watcher: dns_watcher::DnsServerWatcher::new(Arc::new(Mutex::new(dns_rx))),
            socket_provider: socket_provider::SocketProvider::new(mark),
        }
    }
}

enum IncomingService {
    StarnixNetworks(fidl_fuchsia_netpol_socketproxy::StarnixNetworksRequestStream),
    DnsServerWatcher(fidl_fuchsia_netpol_socketproxy::DnsServerWatcherRequestStream),
    PosixSocket(fidl_fuchsia_posix_socket::ProviderRequestStream),
    PosixSocketRaw(fidl_fuchsia_posix_socket_raw::ProviderRequestStream),
}

/// Main entry point for the network socket proxy.
#[fuchsia::main(logging_tags = ["network_socket_proxy"])]
pub async fn main() -> Result<(), anyhow::Error> {
    fuchsia_inspect::component::health().set_starting_up();

    let inspector = fuchsia_inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());

    let proxy = SocketProxy::new().with_inspect(inspector.root(), "root")?;

    let mut fs = ServiceFs::new_local();
    let _: &mut ServiceFsDir<'_, _> = fs
        .dir("svc")
        .add_fidl_service(IncomingService::StarnixNetworks)
        .add_fidl_service(IncomingService::DnsServerWatcher)
        .add_fidl_service(IncomingService::PosixSocket)
        .add_fidl_service(IncomingService::PosixSocketRaw);

    let _: &mut ServiceFs<_> = fs.take_and_serve_directory_handle()?;

    fuchsia_inspect::component::health().set_ok();

    fs.for_each_concurrent(100, |service| async {
        match service {
            IncomingService::StarnixNetworks(stream) => proxy.registry.run_starnix(stream).await,
            IncomingService::DnsServerWatcher(stream) => proxy.dns_watcher.run(stream).await,
            IncomingService::PosixSocket(stream) => proxy.socket_provider.run(stream).await,
            IncomingService::PosixSocketRaw(stream) => proxy.socket_provider.run_raw(stream).await,
        }
        .unwrap_or_else(|e| error!("{e:?}"))
    })
    .await;

    Ok(())
}
