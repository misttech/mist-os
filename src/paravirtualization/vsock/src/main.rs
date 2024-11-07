// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "256"]

use anyhow::{Context as _, Error};
use fidl_fuchsia_hardware_vsock::DeviceMarker;
use fidl_fuchsia_vsock::ConnectorRequestStream;
use fuchsia_component::client::{
    connect_to_named_protocol_at_dir_root, connect_to_protocol_at_path,
};
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt, TryStreamExt};
use vsock_service_config::Config;

use vsock_service_lib as service;

enum IncomingRequest {
    VsockConnection(ConnectorRequestStream),
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    tracing::info!("Starting vsock service");
    let config = Config::take_from_startup_handle();

    if !config.guest_to_host_supported && !config.loopback_supported {
        return Err(anyhow::anyhow!("Invalid config supplied. At least one of guest_to_host_supported or loopback_supported must be true"));
    }

    let guest_vsock_device = if config.guest_to_host_supported {
        const DEV_CLASS_VSOCK: &str = "/dev/class/vsock";
        let vsock_dir =
            fuchsia_fs::directory::open_in_namespace(DEV_CLASS_VSOCK, fuchsia_fs::PERM_READABLE)
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

        Some(
            connect_to_named_protocol_at_dir_root::<DeviceMarker>(&vsock_dir, &path)
                .context("Failed to connect vsock device")?,
        )
    } else {
        None
    };
    let loopback_vsock_device = if config.loopback_supported {
        Some(connect_to_protocol_at_path::<DeviceMarker>(
            "/svc/fuchsia.hardware.vsock.Device-Loopback",
        )?)
    } else {
        None
    };

    let (service, event_loops) = service::Vsock::new(guest_vsock_device, loopback_vsock_device)
        .await
        .context("Failed to initialize vsock service")?;

    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(IncomingRequest::VsockConnection);

    fs.take_and_serve_directory_handle()?;

    let fut = fs.map(Ok).try_for_each_concurrent(None, |request: IncomingRequest| async {
        match request {
            IncomingRequest::VsockConnection(stream) => {
                service.clone().run_client_connection(stream).await;
                Ok(())
            }
        }
    });

    let _ = futures::try_join!(fut, event_loops)?;
    Ok(())
}
