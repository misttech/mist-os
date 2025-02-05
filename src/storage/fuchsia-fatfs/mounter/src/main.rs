// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl::prelude::*;
use fidl_fuchsia_fs::AdminRequestStream;
use fuchsia_component::server::ServiceFs;
use futures::lock::Mutex;
use futures::prelude::*;
use log::{error, info, warn};
use std::sync::Arc;
use zx::Status;

mod device;
use crate::device::FatDevice;

/// All the services handled by the fatfs implementation.
pub enum Services {
    Admin(AdminRequestStream),
}

pub struct FatServer {
    device: Mutex<Option<FatDevice>>,
}

impl FatServer {
    pub fn new() -> Self {
        FatServer { device: Mutex::new(None) }
    }

    async fn ensure_mounted(&self) -> Result<(), Status> {
        let mut device = self.device.lock().await;
        if device.as_ref().map_or(false, |d| d.is_present()) {
            return Ok(());
        }

        if let Some(device) = device.take() {
            device.scope.shutdown();
            device.scope.wait().await;
            // Clean up the old device.
            device.shut_down().unwrap_or_else(|e| {
                info!("Failed to shut down removed disk (this is probably expected): {:?}", e)
            })
        }

        let fat_device = FatDevice::new()
            .await
            .map_err(|e| {
                warn!("Failed to create fat device: {:?}. Is it correctly formatted?", e);
                Status::IO
            })?
            .ok_or(Status::UNAVAILABLE)?;

        *device = Some(fat_device);
        Ok(())
    }

    async fn handle_admin(&self, mut stream: AdminRequestStream) -> Result<(), Error> {
        match self.ensure_mounted().await {
            Ok(()) => {}
            Err(e) => {
                stream.control_handle().shutdown_with_epitaph(e);
                return Ok(());
            }
        };

        while let Some(req) = stream.try_next().await? {
            let mut device = self.device.lock().await;
            if device.as_ref().map_or(true, |d| !d.is_present()) {
                // Device has gone away.
                stream.control_handle().shutdown_with_epitaph(Status::IO_NOT_PRESENT);
                break;
            }
            let device_ref = device.as_ref().unwrap();
            if let Some(shutdown_responder) = device_ref.handle_admin(&device_ref.scope, req).await
            {
                if let Some(device) = device.take() {
                    device.scope.wait().await;
                    device
                        .shut_down()
                        .unwrap_or_else(|error| error!(error:?; "Failed to shutdown fatfs"));
                }
                let _ = shutdown_responder.send();
            }
        }
        Ok(())
    }

    pub async fn handle(&self, service: Services) {
        match service {
            Services::Admin(stream) => self.handle_admin(stream).await,
        }
        .unwrap_or_else(|e| error!("{e:?}"));
    }
}

async fn run() -> Result<(), Error> {
    let mut fs: ServiceFs<_> = ServiceFs::new();

    fs.add_fidl_service(Services::Admin);
    fs.take_and_serve_directory_handle()?;

    let device = Arc::new(FatServer::new());

    const MAX_CONCURRENT: usize = 10_000;
    fs.for_each_concurrent(MAX_CONCURRENT, |request| device.handle(request)).await;

    Ok(())
}

#[fuchsia::main(threads = 10)]
async fn main() {
    run().await.unwrap_or_else(|e| error!("Error while running fatfs mounter: {:?}", e));
}
