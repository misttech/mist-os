// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "256"]

use {
    anyhow::{Context as _, Error},
    fidl_fuchsia_hardware_vsock::DeviceMarker,
    fuchsia_async as fasync,
    fuchsia_component::client::connect_to_named_protocol_at_dir_root,
    fuchsia_component::server::ServiceFs,
    fuchsia_fs::OpenFlags,
    futures::TryStreamExt,
    futures::{StreamExt, TryFutureExt},
};

use vsock_service_lib as service;

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    tracing::info!("Starting vsock service");

    const DEV_CLASS_VSOCK: &str = "/dev/class/vsock";
    let vsock_dir =
        fuchsia_fs::directory::open_in_namespace(DEV_CLASS_VSOCK, OpenFlags::RIGHT_READABLE)
            .context("Open vsock dir")?;
    let path = device_watcher::watch_for_files(&vsock_dir)
        .await
        .with_context(|| format!("Watching for files in {}", DEV_CLASS_VSOCK))?
        .try_next()
        .await
        .with_context(|| format!("Getting a file from {}", DEV_CLASS_VSOCK))?;
    let path = path.ok_or(anyhow::anyhow!("Could not find device in {}", DEV_CLASS_VSOCK))?;
    let path = path.to_str().ok_or(anyhow::anyhow!("Expected valid utf-8 device name"))?;
    let path = format!("{path}/device_protocol");

    let dev = connect_to_named_protocol_at_dir_root::<DeviceMarker>(&vsock_dir, &path)
        .context("Failed to connect vsock device")?;

    let (service, event_loop) =
        service::Vsock::new(dev).await.context("Failed to initialize vsock service")?;

    let service_clone = service.clone();
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(move |stream| {
        fasync::Task::spawn(
            service_clone
                .clone()
                .run_client_connection(stream)
                .unwrap_or_else(|err| tracing::info!("Error {} during client connection", err)),
        )
        .detach();
    });
    fs.take_and_serve_directory_handle()?;

    // Spawn the services server with a wrapper to discard the return value.
    fasync::Task::spawn(fs.collect()).detach();

    // Run the event loop until completion. The event loop only terminates
    // with an error.
    event_loop.await?;
    Ok(())
}
