// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod device;
mod device_watch;
mod inspect;
mod service;
mod watchable_map;
mod watcher_service;

use anyhow::Error;
use fuchsia_async as fasync;
use fuchsia_component::server::{ServiceFs, ServiceObjLocal};
use fuchsia_inspect::{Inspector, InspectorConfig};
use futures::channel::mpsc;
use futures::future::{try_join4, BoxFuture};
use futures::{StreamExt, TryFutureExt};
use std::sync::Arc;
use tracing::{error, info};

const PHY_PATH: &str = "/dev/class/wlanphy";

#[allow(clippy::too_many_arguments, reason = "mass allow for https://fxbug.dev/381896734")]
async fn serve_fidl(
    mut fs: ServiceFs<ServiceObjLocal<'_, ()>>,
    phys: Arc<device::PhyMap>,
    ifaces: Arc<device::IfaceMap>,
    watcher_service: watcher_service::WatcherService<device::PhyDevice, device::IfaceDevice>,
    new_iface_sink: mpsc::UnboundedSender<device::NewIface>,
    iface_counter: Arc<service::IfaceCounter>,
    ifaces_tree: Arc<inspect::IfacesTree>,
    cfg: wlandevicemonitor_config::Config,
) -> Result<(), Error> {
    fs.dir("svc").add_fidl_service(move |reqs| {
        let fut = service::serve_monitor_requests(
            reqs,
            phys.clone(),
            ifaces.clone(),
            watcher_service.clone(),
            new_iface_sink.clone(),
            iface_counter.clone(),
            Arc::clone(&ifaces_tree),
            wlandevicemonitor_config::Config { ..cfg },
        )
        .unwrap_or_else(|e| error!("error serving device monitor API: {}", e));
        fasync::Task::spawn(fut).detach()
    });
    fs.take_and_serve_directory_handle()?;
    fs.collect::<()>().await;
    Ok(())
}

fn serve_phys(
    phys: Arc<device::PhyMap>,
    inspect_tree: Arc<inspect::WlanMonitorTree>,
) -> BoxFuture<'static, Result<std::convert::Infallible, Error>> {
    info!("Serving real device environment");
    let fut = device::serve_phys(phys, inspect_tree, PHY_PATH);
    Box::pin(fut)
}

#[fasync::run_singlethreaded]
async fn main() -> Result<(), Error> {
    diagnostics_log::initialize(
        diagnostics_log::PublishOptions::default()
            .tags(&["wlan"])
            .enable_metatag(diagnostics_log::Metatag::Target),
    )?;
    info!("Starting");

    let (phys, phy_events) = device::PhyMap::new();
    let phys = Arc::new(phys);
    let (ifaces, iface_events) = device::IfaceMap::new();
    let ifaces = Arc::new(ifaces);

    let (watcher_service, watcher_fut) =
        watcher_service::serve_watchers(phys.clone(), ifaces.clone(), phy_events, iface_events);

    let fs = ServiceFs::new_local();

    let inspector = Inspector::new(InspectorConfig::default().size(inspect::VMO_SIZE_BYTES));
    let _inspect_server_task =
        inspect_runtime::publish(&inspector, inspect_runtime::PublishOptions::default());
    let cfg = wlandevicemonitor_config::Config::take_from_startup_handle();
    inspector.root().record_child("config", |config_node| cfg.record_inspect(config_node));
    let ifaces_tree = Arc::new(inspect::IfacesTree::new(inspector.clone()));
    let inspect_tree = Arc::new(inspect::WlanMonitorTree::new(inspector));

    let phy_server = serve_phys(phys.clone(), inspect_tree.clone());

    let iface_counter = Arc::new(service::IfaceCounter::new());

    let (new_iface_sink, new_iface_stream) = mpsc::unbounded();
    let fidl_fut = serve_fidl(
        fs,
        phys.clone(),
        ifaces.clone(),
        watcher_service,
        new_iface_sink,
        iface_counter,
        Arc::clone(&ifaces_tree),
        cfg,
    );

    let new_iface_fut = service::handle_new_iface_stream(
        phys.clone(),
        ifaces.clone(),
        ifaces_tree,
        new_iface_stream,
    );

    let ((), (), (), ()) = try_join4(
        fidl_fut,
        phy_server.map_ok(|_: std::convert::Infallible| ()),
        watcher_fut.map_ok(|_: std::convert::Infallible| ()),
        new_iface_fut,
    )
    .await?;
    error!("Exiting");
    Ok(())
}
