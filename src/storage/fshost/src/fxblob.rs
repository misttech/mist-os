// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::DeviceTag;
use crate::environment::{Container, Environment, Filesystem, FilesystemLauncher, FxfsContainer};
use anyhow::{anyhow, Context, Error, Result};
use fidl_fuchsia_update_verify as fupdate;
use fs_management::filesystem::ServingMultiVolumeFilesystem;
use fuchsia_async::TimeoutExt;
use futures::lock::Mutex;
use futures::StreamExt;
use std::sync::Arc;
use vfs::service;
use zx::{self as zx, MonotonicDuration};

/// Make a new vfs service node that implements fuchsia.update.verify.BlobfsVerifier
pub fn blobfs_verifier_service() -> Arc<service::Service> {
    service::host(move |mut stream: fupdate::BlobfsVerifierRequestStream| async move {
        while let Some(request) = stream.next().await {
            match request {
                Ok(fupdate::BlobfsVerifierRequest::Verify { responder, .. }) => {
                    // TODO(https://fxbug.dev/42077105): Implement by calling out
                    // to Fxfs' blob volume.
                    responder.send(Ok(())).unwrap_or_else(|e| {
                        tracing::error!("failed to send Verify response. error: {:?}", e);
                    });
                }
                Err(e) => {
                    tracing::error!("BlobfsVerifier server failed: {:?}", e);
                    return;
                }
            }
        }
    })
}

/// Make a new vfs service node that implements fuchsia.update.ComponentOtaHealthCheck
pub fn ota_health_check_service() -> Arc<service::Service> {
    service::host(move |mut stream: fupdate::ComponentOtaHealthCheckRequestStream| async move {
        while let Some(request) = stream.next().await {
            match request {
                Ok(fupdate::ComponentOtaHealthCheckRequest::GetHealthStatus {
                    responder, ..
                }) => {
                    // TODO(https://fxbug.dev/42077105): Implement by calling out
                    // to Fxfs' blob volume.
                    responder.send(fupdate::HealthStatus::Healthy).unwrap_or_else(|e| {
                        tracing::error!("failed to send GetHealthStatus response. error: {:?}", e);
                    });
                }
                Err(e) => {
                    tracing::error!("ComponentOtaHealthCheck server failed: {:?}", e);
                    return;
                }
            }
        }
    })
}

const FIND_PARTITION_DURATION: MonotonicDuration = MonotonicDuration::from_seconds(10);

/// Mounts (or formats) the data volume in Fxblob.  Assumes the partition is already formatted.
pub async fn mount_or_format_data(
    environment: &Arc<Mutex<dyn Environment>>,
    launcher: &FilesystemLauncher,
) -> Result<(ServingMultiVolumeFilesystem, Filesystem), Error> {
    // Find the device via our own matcher.
    let registered_devices = environment.lock().await.registered_devices().clone();
    let block_connector = registered_devices
        .get_block_connector(DeviceTag::FxblobOnRecovery)
        .on_timeout(FIND_PARTITION_DURATION, || {
            Err(anyhow!("timed out waiting for fxfs partition"))
        })
        .await
        .context("failed to get block connector for fxfs partition")?;
    let mut container = Box::new(FxfsContainer::new(
        launcher.serve_fxblob(block_connector).await.context("serving Fxblob")?,
    ));
    let data = container.serve_data(&launcher).await.context("serving data from Fxblob")?;

    Ok((container.into_fs(), data))
}
