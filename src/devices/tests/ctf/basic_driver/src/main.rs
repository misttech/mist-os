// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashSet;

use anyhow::{anyhow, Error, Result};
use fidl::endpoints::{create_endpoints, create_proxy, DiscoverableProtocolMarker, ServiceMarker};
use fidl_fuchsia_basicdriver_ctftest as ctf;
use fidl_fuchsia_driver_test as fdt;
use fidl_fuchsia_driver_testing as ftest;
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_component::client::{connect_to_protocol, connect_to_service_instance_at};
use fuchsia_component::server::ServiceFs;
use fuchsia_fs::directory::WatchEvent;
use futures::channel::mpsc;
use futures::prelude::*;
use realm_proxy_client::{extend_namespace, InstalledNamespace};
use tracing::info;

async fn run_waiter_server(mut stream: ctf::WaiterRequestStream, mut sender: mpsc::Sender<()>) {
    while let Some(ctf::WaiterRequest::Ack { .. }) = stream.try_next().await.expect("Stream failed")
    {
        info!("Received Ack request");
        sender.try_send(()).expect("Sender failed")
    }
}

async fn run_offers_server(
    offers_server: fidl::endpoints::ServerEnd<fio::DirectoryMarker>,
    sender: mpsc::Sender<()>,
) -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(move |stream: ctf::WaiterRequestStream| {
        fasync::Task::spawn(run_waiter_server(stream, sender.clone())).detach()
    });
    // Serve the outgoing services
    fs.serve_connection(offers_server)?;
    Ok(fs.collect::<()>().await)
}

async fn create_realm(options: ftest::RealmOptions) -> Result<InstalledNamespace> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (dict_client, dict_server) = create_endpoints();
    realm_factory
        .create_realm2(options, dict_server)
        .await?
        .map_err(realm_proxy_client::Error::OperationError)?;
    let ns = extend_namespace(realm_factory, dict_client).await?;
    Ok(ns)
}

// TODO(b/324834340): Re-enable once no longer flaky
#[ignore]
#[fuchsia::test]
async fn test_basic_driver() -> Result<()> {
    let (pkg_client, pkg_server) = create_endpoints();
    fuchsia_fs::directory::open_channel_in_namespace(
        "/pkg",
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        pkg_server,
    )
    .expect("Could not open /pkg");

    let (offers_client, offers_server) = create_endpoints();

    let (devfs_client, devfs_server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()?;

    let realm_options = ftest::RealmOptions {
        driver_test_realm_start_args: Some(fdt::RealmArgs {
            pkg: Some(pkg_client),
            offers: Some(vec![fdt::Offer {
                protocol_name: ctf::WaiterMarker::PROTOCOL_NAME.to_string(),
                collection: fdt::Collection::PackageDrivers,
            }]),
            exposes: Some(vec![fdt::Expose {
                service_name: ctf::ServiceMarker::SERVICE_NAME.to_string(),
                collection: fdt::Collection::PackageDrivers,
            }]),
            ..Default::default()
        }),
        offers_client: Some(offers_client),
        dev_topological: Some(devfs_server),
        ..Default::default()
    };
    let test_ns = create_realm(realm_options).await?;
    info!("connected to the test realm!");

    // Setup our offers to provide the Waiter, and wait to receive the waiter ack event.
    let (sender, mut receiver) = mpsc::channel(1);
    let receiver_next = receiver.next().fuse();
    let offers_server = run_offers_server(offers_server, sender).fuse();
    futures::pin_mut!(receiver_next);
    futures::pin_mut!(offers_server);
    futures::select! {
        _ = receiver_next => {}
        _ = offers_server => { panic!("should not quit offers_server."); }
    }

    // Check to make sure our topological devfs connections is working.
    device_watcher::recursive_wait(&devfs_client, "sys/test").await?;

    // Connect to the device. We have already received an ack from the driver, but sometimes
    // seeing the item in the service directory doesn't happen immediately. So we wait for a
    // corresponding directory watcher event.
    let (service, server) = create_proxy::<fio::DirectoryMarker>().unwrap();
    fdio::open(
        &format!("{}/{}", test_ns.prefix(), ctf::ServiceMarker::SERVICE_NAME),
        fio::OpenFlags::RIGHT_READABLE,
        server.into_channel(),
    )
    .unwrap();
    let mut watcher = fuchsia_fs::directory::Watcher::new(&service).await?;
    let mut instances: HashSet<String> = Default::default();
    let mut event = None;
    while instances.len() != 1 {
        event = watcher.next().await;
        let Some(Ok(ref message)) = event else { break };
        let filename = message.filename.as_path().to_str().unwrap().to_owned();
        if filename == "." {
            continue;
        }
        match message.event {
            WatchEvent::ADD_FILE | WatchEvent::EXISTING => _ = instances.insert(filename),
            WatchEvent::REMOVE_FILE => _ = instances.remove(&filename),
            WatchEvent::IDLE => {}
            WatchEvent::DELETED => break,
        }
    }
    if instances.len() != 1 {
        return Err(anyhow!(
            "Expected to find one instance within the service directory. \
            Last event: {event:?}. Instances: {instances:?}"
        ));
    }

    let service_instance = connect_to_service_instance_at::<ctf::ServiceMarker>(
        test_ns.prefix(),
        instances.iter().next().unwrap(),
    )?;
    let device = service_instance.connect_to_device()?;

    // Talk to the device!
    let pong = device.ping().await?;
    assert_eq!(42, pong);
    Ok(())
}
