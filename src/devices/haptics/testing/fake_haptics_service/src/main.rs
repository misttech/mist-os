// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The fake haptics service's goal is to provide a component that offers a fake
//! fuchsia.hardware.haptics.Service` FIDL service instance.
//!
//! Currently, it responds to `StartVibration()` and `StopVibration()` by returning
//! `ZX_ERR_NOT_IMPLEMENTED`.

use anyhow::Context;
use fidl_fuchsia_hardware_haptics::{DeviceRequest, DeviceRequestStream, ServiceRequest};
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt, TryFutureExt, TryStreamExt};
use log::error;

async fn handle_device_requests(mut requests: DeviceRequestStream) -> anyhow::Result<()> {
    while let Some(request) = requests.try_next().await.context("Failed to get request")? {
        match request {
            DeviceRequest::StartVibration { responder } => {
                responder.send(Ok(())).context("Failed to send response")?;
            }
            DeviceRequest::StopVibration { responder } => {
                responder.send(Ok(())).context("Failed to send response")?;
            }
            DeviceRequest::_UnknownMethod { ordinal, .. } => {
                error!("Received unknown method {}", ordinal);
            }
        };
    }
    Ok(())
}

enum IncomingService {
    Haptics(ServiceRequest),
}

#[fuchsia::main]
async fn main() -> anyhow::Result<()> {
    // Initialize the outgoing services provided by this component
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service_instance("default", IncomingService::Haptics);
    fs.take_and_serve_directory_handle()?;

    fs.for_each_concurrent(1, |IncomingService::Haptics(ServiceRequest::Device(device))| {
        handle_device_requests(device).unwrap_or_else(|e| println!("{:?}", e))
    })
    .await;

    Ok(())
}
