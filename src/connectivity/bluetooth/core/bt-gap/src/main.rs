// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "512"]

use anyhow::{format_err, Context as _, Error};
use async_helpers::hanging_get::asynchronous as hanging_get;
use fidl::endpoints::{DiscoverableProtocolMarker, ProtocolMarker};
use fidl_fuchsia_bluetooth::Appearance;
use fidl_fuchsia_bluetooth_bredr::ProfileMarker;
use fidl_fuchsia_bluetooth_gatt::Server_Marker;
use fidl_fuchsia_bluetooth_gatt2::{LocalServiceRequest, Server_Marker as Server_Marker2};
use fidl_fuchsia_bluetooth_le::{CentralMarker, PeripheralMarker};
use fidl_fuchsia_device::NameProviderMarker;
use fuchsia_async as fasync;
use fuchsia_bluetooth::constants::{DEV_DIR, HOST_DEVICE_DIR};
use fuchsia_component::{client::connect_to_protocol, server::ServiceFs};
use futures::channel::mpsc;
use futures::future::BoxFuture;
use futures::{try_join, FutureExt, StreamExt, TryFutureExt, TryStreamExt};
use std::collections::HashMap;
use tracing::{error, info, warn};

use crate::{
    generic_access_service::GenericAccessService,
    host_dispatcher::{HostDispatcher, HostService, HostService::*},
    services::host_watcher,
    watch_peers::PeerWatcher,
};

mod build_config;
mod generic_access_service;
mod host_device;
mod host_dispatcher;
mod services;
mod store;
#[cfg(test)]
mod test;
mod types;
mod watch_peers;

const BT_GAP_COMPONENT_ID: &'static str = "bt-gap";

#[fuchsia::main(logging_tags = ["bt-gap"])]
async fn main() -> Result<(), Error> {
    info!("Starting bt-gap...");
    let bt_gap = BtGap::init().await.context("Error starting bt-gap").map_err(|e| {
        error!("{:?}", e);
        e
    })?;

    bt_gap.run().await.context("Error running bt-gap").map_err(|e| {
        error!("{:?}", e);
        e
    })
}

/// Returns the device host name that we assign as the local Bluetooth device name by default.
async fn get_host_name() -> types::Result<String> {
    // Obtain the local device name to assign it as the default Bluetooth name,
    let name_provider = connect_to_protocol::<NameProviderMarker>()?;
    name_provider
        .get_device_name()
        .await?
        .map_err(|e| format_err!("failed to obtain host name: {:?}", e).into())
}

fn host_service_handler(
    dispatcher: &HostDispatcher,
    service_name: &'static str,
    service: HostService,
) -> impl FnMut(fuchsia_zircon::Channel) -> Option<()> {
    let dispatcher = dispatcher.clone();
    move |chan| {
        info!("Connecting {} to Host Device", service_name);
        fasync::Task::spawn(dispatcher.clone().request_host_service(chan, service)).detach();
        None
    }
}

/// The constituent parts of the bt-gap application.
struct BtGap {
    hd: HostDispatcher,
    inspect: fuchsia_inspect::Inspector,
    /// The generic access service requests
    gas_requests: mpsc::Receiver<LocalServiceRequest>,
    run_watch_peers: BoxFuture<'static, Result<(), Error>>,
    run_watch_hosts: BoxFuture<'static, Result<(), Error>>,
}

impl BtGap {
    /// Initialize bt-gap, in particular creating the core HostDispatcher object
    async fn init() -> Result<Self, Error> {
        info!("Initializing bt-gap...");
        let inspect = fuchsia_inspect::Inspector::default();
        let stash_inspect = inspect.root().create_child("persistent");
        info!("Initializing data store from Stash...");
        let stash = store::stash::init_stash(BT_GAP_COMPONENT_ID, stash_inspect)
            .await
            .context("Error initializing Stash service")?;
        info!("Data store initialized successfully");

        let (gas_channel_sender, gas_requests) = mpsc::channel(0);

        // Initialize a HangingGetBroker to process watch_peers requests
        let watch_peers_broker = hanging_get::HangingGetBroker::new(
            HashMap::new(),
            PeerWatcher::observe,
            hanging_get::DEFAULT_CHANNEL_SIZE,
        );
        let watch_peers_publisher = watch_peers_broker.new_publisher();
        let watch_peers_registrar = watch_peers_broker.new_registrar();

        // Initialize a HangingGetBroker to process watch_hosts requests
        let watch_hosts_broker = hanging_get::HangingGetBroker::new(
            Vec::new(),
            host_watcher::observe_hosts,
            hanging_get::DEFAULT_CHANNEL_SIZE,
        );
        let watch_hosts_publisher = watch_hosts_broker.new_publisher();
        let watch_hosts_registrar = watch_hosts_broker.new_registrar();

        // Process the watch_peers broker in the background
        let run_watch_peers = watch_peers_broker
            .run()
            .map(|()| Err::<(), Error>(format_err!("WatchPeers broker terminated unexpectedly")))
            .boxed();
        // Process the watch_hosts broker in the background
        let run_watch_hosts = watch_hosts_broker
            .run()
            .map(|()| Err::<(), Error>(format_err!("WatchHosts broker terminated unexpectedly")))
            .boxed();

        let hd = HostDispatcher::new(
            Appearance::Display,
            stash,
            inspect.root().create_child("system"),
            gas_channel_sender,
            watch_peers_publisher,
            watch_peers_registrar,
            watch_hosts_publisher,
            watch_hosts_registrar,
        );

        info!("bt-gap successfully initialized.");
        Ok(BtGap { hd, inspect, gas_requests, run_watch_peers, run_watch_hosts })
    }

    /// Run continuous tasks that are expected to live until bt-gap terminates
    async fn run(self) -> Result<(), Error> {
        let watch_for_hosts = run_host_watcher(&self.hd);

        let set_local_name = {
            let hd = self.hd.clone();
            async move {
                info!("Obtaining system host name...");
                if let Err(e) = get_host_name()
                    .and_then(|name| hd.set_name(name, host_dispatcher::NameReplace::Keep))
                    .await
                {
                    warn!("Error setting Bluetooth host name from system: {:?}", e);
                }
                Ok(())
            }
        };

        let run_generic_access_service =
            GenericAccessService::build(&self.hd, self.gas_requests).run().map(|()| {
                Err::<(), Error>(format_err!("Generic Access Server terminated unexpectedly"))
            });

        let serve_fidl = serve_fidl(self.hd.clone(), self.inspect);

        try_join!(
            set_local_name,
            serve_fidl,
            watch_for_hosts,
            run_generic_access_service,
            self.run_watch_peers,
            self.run_watch_hosts,
        )
        .map(|((), (), (), (), (), ())| ())
    }
}

/// Continuously watch the file system for bt-host devices being added or removed
async fn run_host_watcher(hd: &HostDispatcher) -> Result<(), Error> {
    let dir = format!("{}/{}", DEV_DIR, HOST_DEVICE_DIR);
    let directory = fuchsia_fs::directory::open_in_namespace(&dir, fuchsia_fs::OpenFlags::empty())?;
    let watcher = fuchsia_fs::directory::Watcher::new(&directory).await?;
    let directory = &directory;
    watcher
        .map(|result| result.context("failed to watch hosts"))
        .try_for_each(|fuchsia_fs::directory::WatchMessage { event, filename }| async move {
            use fuchsia_fs::directory::WatchEvent;
            match event {
                WatchEvent::EXISTING | WatchEvent::ADD_FILE => {
                    if filename != std::path::Path::new(".") {
                        let path = filename.to_str().expect("utf-8 path");
                        hd.add_host_by_path(directory, &path)
                            .await
                            .with_context(|| format!("adding bt-host device {path}"))?;
                    }
                }
                WatchEvent::REMOVE_FILE => {
                    let path = filename.to_str().expect("utf-8 path");
                    hd.rm_device(&path).await;
                }
                WatchEvent::IDLE => {}
                e => {
                    warn!("unrecognized host watch event: {e:?}");
                }
            }
            Ok(())
        })
        .await
}

/// Serve the FIDL protocols offered by bt-gap
async fn serve_fidl(hd: HostDispatcher, inspect: fuchsia_inspect::Inspector) -> Result<(), Error> {
    let mut fs = ServiceFs::new();

    // serve bt-gap inspect VMO
    let _inspect_server_task =
        inspect_runtime::publish(&inspect, inspect_runtime::PublishOptions::default());

    let _ = fs
        .dir("svc")
        .add_service_at(
            CentralMarker::PROTOCOL_NAME,
            host_service_handler(&hd, CentralMarker::DEBUG_NAME, LeCentral),
        )
        .add_service_at(
            PeripheralMarker::PROTOCOL_NAME,
            host_service_handler(&hd, PeripheralMarker::DEBUG_NAME, LePeripheral),
        )
        .add_service_at(
            Server_Marker::PROTOCOL_NAME,
            host_service_handler(&hd, Server_Marker::DEBUG_NAME, LeGatt),
        )
        .add_service_at(
            Server_Marker2::PROTOCOL_NAME,
            host_service_handler(&hd, Server_Marker2::DEBUG_NAME, LeGatt2),
        )
        .add_service_at(
            ProfileMarker::PROTOCOL_NAME,
            host_service_handler(&hd, ProfileMarker::PROTOCOL_NAME, Profile),
        )
        // TODO(fxbug.dev/1496) - according fuchsia.bluetooth.sys/bootstrap.fidl, the bootstrap service should
        // only be available before initialization, and only allow a single commit before becoming
        // unservicable. This behavior interacts with parts of Bluetooth lifecycle and component
        // framework design that are not yet complete. For now, we provide the service to whomever
        // asks, whenever, but clients should not rely on this. The implementation will change once
        // we have a better solution.
        .add_fidl_service(|request_stream| {
            let hd = hd.clone();
            fasync::Task::spawn(
                services::bootstrap::run(hd, request_stream)
                    .unwrap_or_else(|e| warn!("Bootstrap service failed: {:?}", e)),
            )
            .detach();
        })
        .add_fidl_service(|request_stream| {
            let hd = hd.clone();
            fasync::Task::spawn(
                services::access::run(hd, request_stream)
                    .unwrap_or_else(|e| warn!("Access service failed: {:?}", e)),
            )
            .detach();
        })
        .add_fidl_service(|request_stream| {
            let hd = hd.clone();
            fasync::Task::spawn(
                services::configuration::run(hd, request_stream)
                    .unwrap_or_else(|e| warn!("Configuration service failed: {:?}", e)),
            )
            .detach();
        })
        .add_fidl_service(|request_stream| {
            let hd = hd.clone();
            fasync::Task::spawn(
                services::host_watcher::run(hd, request_stream)
                    .unwrap_or_else(|e| warn!("HostWatcher service failed: {:?}", e)),
            )
            .detach();
        })
        .add_fidl_service(|request_stream| {
            let hd = hd.clone();
            fasync::Task::spawn(
                services::pairing::run(hd, request_stream)
                    .unwrap_or_else(|e| warn!("Pairing service failed: {:?}", e)),
            )
            .detach();
        });
    let _ = fs.take_and_serve_directory_handle()?;
    fs.collect::<()>().await;
    Ok(())
}
