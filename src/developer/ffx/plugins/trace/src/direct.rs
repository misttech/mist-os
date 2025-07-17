// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::TraceData;
use anyhow::Result;
use fidl_fuchsia_developer_ffx::TraceOptions;
use fidl_fuchsia_tracing_controller::{ProvisionerProxy, TraceConfig};
use futures::Future;
use std::pin::Pin;
use std::time::Duration;
use trace_task::TraceTask;

pub(crate) async fn trace(
    proxy: ProvisionerProxy,
    output: String,
    options: TraceOptions,
    trace_config: TraceConfig,
) -> Result<TraceTask> {
    let on_complete =
        move || Box::pin(async move {}) as Pin<Box<dyn Future<Output = ()> + 'static>>;

    let duration = options.duration_ns.map(|d| Duration::from_nanos(d as u64));
    let task = TraceTask::new(
        "ffx-trace-direct".into(),
        output.clone(),
        trace_config.clone(),
        duration,
        options
            .triggers
            .map(|tv| {
                tv.iter()
                    .map(|t| trace_task::Trigger {
                        action: t.action.as_ref().map(|_| trace_task::TriggerAction::Terminate),
                        alert: t.alert.clone(),
                    })
                    .collect()
            })
            .unwrap_or(vec![]),
        proxy,
        on_complete,
    )
    .await?;

    if let Some(trace_duration) = duration {
        fuchsia_async::Timer::new(trace_duration).await;
    }
    Ok(task)
}

pub(crate) async fn stop_tracing(task: TraceTask) -> Result<TraceData> {
    Ok(TraceData {
        output_file: task.output_file(),
        categories: task.config().categories.clone().unwrap_or(vec![]),
        stop_result: task.shutdown().await?,
    })
}
