// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::Device;
use crate::environment::Environment;
use crate::{matcher, service};
use anyhow::{format_err, Error};
use fs_management::format::DiskFormat;
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::StreamExt;
use std::collections::HashSet;
use std::sync::Arc;

pub struct Manager {
    matcher: matcher::Matchers,
    environment: Arc<Mutex<dyn Environment>>,
    /// Holds a set of topological paths that have already been processed and
    /// should be ignored when matching. When matched, the ignored paths are removed from the set.
    /// (i.e. The device is ignored only once.)
    matcher_lock: Arc<Mutex<HashSet<String>>>,
}

impl Manager {
    pub fn new(
        config: &fshost_config::Config,
        environment: Arc<Mutex<dyn Environment>>,
        matcher_lock: Arc<Mutex<HashSet<String>>>,
    ) -> Self {
        Manager { matcher: matcher::Matchers::new(config), environment, matcher_lock }
    }

    /// The main loop of fshost. Watch for new devices, match them against filesystems we expect,
    /// and then launch them appropriately.
    pub async fn device_handler(
        &mut self,
        device_stream: impl futures::Stream<Item = Box<dyn Device>>,
        mut shutdown_rx: mpsc::Receiver<service::FshostShutdownResponder>,
    ) -> Result<service::FshostShutdownResponder, Error> {
        let mut device_stream = Box::pin(device_stream).fuse();
        let mut ignored_paths = HashSet::new();
        loop {
            // Wait for the next device to come in, or the shutdown signal to arrive.
            let mut device = futures::select! {
                responder = shutdown_rx.next() => {
                    let responder = responder
                        .ok_or_else(|| format_err!("shutdown signal stream ended unexpectedly"))?;
                    return Ok(responder);
                },
                maybe_device = device_stream.next() => {
                    if let Some(device) = maybe_device {
                        device
                    } else {
                        anyhow::bail!("block watcher returned none unexpectedly");
                    }
                },
            };

            for path in (*self.matcher_lock.lock().await).drain() {
                ignored_paths.insert(path);
            }
            let topological_path = device.topological_path().to_string();
            if ignored_paths.remove(&topological_path) {
                log::info!(
                    topological_path = topological_path.as_str();
                    "Skipping explicitly ignored device."
                );
                continue;
            }

            let content_format = device.content_format().await.unwrap_or(DiskFormat::Unknown);
            let label = device.partition_label().await.ok().map(|s| s.to_string());
            let type_guid =
                device.partition_type().await.ok().map(|guid| uuid::Uuid::from_bytes(guid.clone()));
            log::info!(
                topological_path=topological_path.as_str(),
                path:% = device.path(),
                content_format:?,
                label:?,
                type_guid:?,
                is_ramdisk = device.is_fshost_ramdisk();
                "Matching device"
            );

            let device_path = device.path().to_string();
            match self.matcher.match_device(device, &mut *self.environment.lock().await).await {
                Ok(true) => {}
                // TODO(https://fxbug.dev/42069366): //src/tests/installer and //src/tests/femu look for
                // "/dev/class/block/008 ignored"
                Ok(false) => log::info!(path:% = device_path; "ignored"),
                Err(error) => {
                    log::error!(
                        path:% = device_path,
                        error:?;
                        "Failed to match device",
                    );
                }
            }
        }
    }

    pub async fn shutdown(self) -> Result<(), Error> {
        self.environment.lock().await.shutdown().await
    }
}
