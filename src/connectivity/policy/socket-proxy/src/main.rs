// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of the network socket proxy.
//!
//! Runs proxied versions of fuchsia.posix.socket.Provider and fuchsia.posix.socket.raw.Provider.
//! Exposes fuchsia.netpol.socketproxy.StarnixNetworks and
//! fuchsia.netpol.socketproxy.DnsServerWatcher.

use fidl_fuchsia_netpol_socketproxy as fnp_socketproxy;
use fidl_fuchsia_posix_socket::{self as fposix_socket, OptionalUint32};
use fuchsia_component::server::{ServiceFs, ServiceFsDir};
use fuchsia_inspect::health::Reporter;
use fuchsia_inspect_derive::{Inspect, WithInspect as _};
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::StreamExt as _;
use std::sync::Arc;
use tracing::error;

mod registry;

struct SocketMarks {
    mark_1: OptionalUint32,
    #[allow(unused)]
    mark_2: OptionalUint32,
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

    #[inspect(skip)]
    _dns_rx: Arc<Mutex<mpsc::Receiver<Vec<fnp_socketproxy::DnsServerList>>>>,
}

impl SocketProxy {
    fn new() -> Self {
        let mark = Arc::new(Mutex::new(SocketMarks::default()));
        let (dns_tx, dns_rx) = mpsc::channel(1);
        let _dns_rx = Arc::new(Mutex::new(dns_rx));
        Self { registry: registry::Registry::new(mark.clone(), dns_tx), _dns_rx }
    }
}

enum IncomingService {
    StarnixNetworks(fidl_fuchsia_netpol_socketproxy::StarnixNetworksRequestStream),
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
    let _: &mut ServiceFsDir<'_, _> =
        fs.dir("svc").add_fidl_service(IncomingService::StarnixNetworks);

    let _: &mut ServiceFs<_> = fs.take_and_serve_directory_handle()?;

    fuchsia_inspect::component::health().set_ok();

    fs.for_each_concurrent(100, |service| async {
        match service {
            IncomingService::StarnixNetworks(stream) => proxy.registry.run_starnix(stream),
        }
        .await
        .unwrap_or_else(|e| error!("{e:?}"))
    })
    .await;

    Ok(())
}
