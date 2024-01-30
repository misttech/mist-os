// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error, Result};
use fidl::endpoints::{create_endpoints, DiscoverableProtocolMarker, ServiceMarker};
use fidl_fuchsia_basicdriver_ctftest as ctf;
use fidl_fuchsia_driver_test as fdt;
use fidl_fuchsia_driver_testing as ftest;
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::ServiceFs;
use futures::channel::mpsc;
use futures::prelude::*;
use realm_proxy_client::RealmProxyClient;
use tracing::info;

async fn run_waiter_server(mut stream: ctf::WaiterRequestStream, mut sender: mpsc::Sender<()>) {
    while let Some(ctf::WaiterRequest::Ack { .. }) = stream.try_next().await.expect("Stream failed")
    {
        info!("Received Ack request");
        sender.try_send(()).expect("Sender failed")
    }
}

async fn query_service_instances(service_dir: &fio::DirectoryProxy) -> Result<Vec<String>, Error> {
    let instances = fuchsia_fs::directory::readdir(service_dir)
        .await
        .map_err(|e| anyhow!("{e:?}"))?
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();

    Ok(instances)
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

async fn create_realm(options: ftest::RealmOptions) -> Result<RealmProxyClient> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();
    realm_factory
        .create_realm(options, server)
        .await?
        .map_err(realm_proxy_client::Error::OperationError)?;
    Ok(RealmProxyClient::from(client))
}

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
            use_driver_framework_v2: Some(true),
            pkg: Some(pkg_client),
            offers: Some(vec![fdt::Offer {
                protocol_name: ctf::WaiterMarker::PROTOCOL_NAME.to_string(),
                collection: fdt::Collection::PackageDrivers,
            }]),
            exposes: Some(vec![fdt::Expose {
                service_name: ctf::ServiceMarker::SERVICE_NAME.to_string(),
                collection: fdt::Collection::PackageDrivers,
            }]),
            driver_urls: Some(vec!["fuchsia-pkg://fuchsia.com/#meta/basic-driver.cm".to_string()]),
            ..Default::default()
        }),
        offers_client: Some(offers_client),
        dev_topological: Some(devfs_server),
        ..Default::default()
    };
    let realm = create_realm(realm_options).await?;
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

    // Connect to the device. No need to loop/wait for the service instance
    // since we have already received an ack from the driver.
    let service = realm.open_service::<ctf::ServiceMarker>().await?;
    let instances = query_service_instances(&service).await?;
    assert_eq!(1, instances.len());
    let service_instance =
        realm.connect_to_service_instance::<ctf::ServiceMarker>(instances.first().unwrap()).await?;
    let device = service_instance.connect_to_device()?;

    // Talk to the device!
    let pong = device.ping().await?;
    assert_eq!(42, pong);
    Ok(())
}
