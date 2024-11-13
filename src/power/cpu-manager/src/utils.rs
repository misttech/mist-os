// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl_fuchsia_hardware_cpu_ctrl as fcpu_ctrl;
use fuchsia_async::{DurationExt, TimeoutExt};
use fuchsia_component::client::{self as fclient, connect_to_service_instance};
use futures::{TryFutureExt, TryStreamExt};
use std::cmp::Reverse;
use std::collections::HashMap;
use zx::MonotonicDuration;

const CPU_DRIVER_TIMEOUT: MonotonicDuration = MonotonicDuration::from_seconds(5);

pub async fn get_cpu_ctrl_proxy(
    node_info: &str,
    total_domain_count: u8,
    perf_rank: u8,
) -> Result<fcpu_ctrl::DeviceProxy, Error> {
    let dir = fclient::open_service::<fcpu_ctrl::ServiceMarker>()
        .expect("failed to open fuchsia.hardware.cpu.ctrl service directory");

    let mut watcher = fuchsia_fs::directory::Watcher::new(&dir)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create watcher: {:?}", e))?;

    let mut proxies = HashMap::new();

    loop {
        let next = watcher
            .try_next()
            .map_err(|e| anyhow::anyhow!("Failed to get next watch message: {e:?}"))
            .on_timeout(CPU_DRIVER_TIMEOUT.after_now(), || {
                Err(anyhow::anyhow!("Timeout waiting for next watcher message."))
            })
            .await?;

        if let Some(watch_msg) = next {
            let filename = watch_msg
                .filename
                .as_path()
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Failed to convert filename to string"))?
                .to_owned();
            if filename != "." {
                if watch_msg.event == fuchsia_fs::directory::WatchEvent::ADD_FILE
                    || watch_msg.event == fuchsia_fs::directory::WatchEvent::EXISTING
                {
                    let proxy =
                        connect_to_service_instance::<fcpu_ctrl::ServiceMarker>(filename.as_str())?
                            .connect_to_device()
                            .map_err(|e| anyhow::anyhow!("Failed to connect to device: {:?}", e))?;

                    let relative_perf = proxy.get_relative_performance().await?.map_err(|e| {
                        anyhow::anyhow!("GetRelativePerformance returned err: {:?}", e)
                    })?;
                    tracing::info!(
                        node_info,
                        instance_filename = filename,
                        relative_perf,
                        "CPU device detected"
                    );
                    if proxies.insert(relative_perf, proxy).is_some() {
                        tracing::warn!(
                            "CPU driver of relative performance {:?} showed up more than once",
                            relative_perf
                        );
                    }

                    if proxies.len() == total_domain_count as usize {
                        // Sort by relative_perf from highest to lowest.
                        let mut proxies_sort = proxies.into_iter().collect::<Vec<_>>();
                        proxies_sort.sort_by_key(|r| Reverse(r.0));
                        return Ok(proxies_sort[perf_rank as usize].1.clone());
                    }
                }
            }
        } else {
            return Err(anyhow::anyhow!("Directory watcher returned None entry."));
        }
    }
}
