// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use fidl::endpoints::{ControlHandle, RequestStream};
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at_path};
use fuchsia_component::server::ServiceFs;
use fuchsia_trace::duration;
use futures::StreamExt;
use resources::Job;
use snapshot::AttributionSnapshot;
use std::sync::Arc;
use traces::CATEGORY_MEMORY_CAPTURE;
use tracing::{error, warn};

use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_kernel as fkernel,
    fidl_fuchsia_memory_attribution as fattribution,
    fidl_fuchsia_memory_attribution_plugin as fattribution_plugin,
};

mod attribution_client;
mod common;
mod resources;
mod snapshot;

/// All FIDL services that are exposed by this component's ServiceFs.
enum Service {
    /// The `fuchsia.memory.heapdump.client.Collector` protocol.
    MemoryMonitor(fattribution_plugin::MemoryMonitorRequestStream),
}

const INTROSPECTOR_PATH: &str = "/svc/fuchsia.component.Introspector.root";

// Enable debug trace:
// 1. set `logging_minimum_severity = "debug"`
// 2. run `fx log --severity trace --moniker core/memory_monitor2`
#[fuchsia::main(logging_minimum_severity = "info")]
async fn main() -> Result<(), Error> {
    let mut service_fs = ServiceFs::new();

    service_fs.dir("svc").add_fidl_service(Service::MemoryMonitor);
    service_fs.take_and_serve_directory_handle()?;

    let attribution_provider = connect_to_protocol::<fattribution::ProviderMarker>()
        .context("Failed to connect to the memory attribution provider")?;
    let introspector =
        connect_to_protocol_at_path::<fcomponent::IntrospectorMarker>(&INTROSPECTOR_PATH)
            .context("Failed to connect to the memory attribution provider")?;
    let root_job = connect_to_protocol::<fkernel::RootJobForInspectMarker>()
        .context("Error connecting to the root job")?
        .get()
        .await?;
    let attribution_client = attribution_client::AttributionClient::new(
        attribution_provider,
        introspector,
        root_job.get_koid().context("Unable to get the root job's koid")?,
    );

    let kernel_stats = connect_to_protocol::<fkernel::StatsMarker>()
        .context("Failed to connect to the kernel stats provider")?;

    // Serves Fuchsia performance trace system.
    // https://fuchsia.dev/fuchsia-src/concepts/kernel/tracing-system
    // Watch trace category and trace kernel memory stats, until this variable goes out of scope.
    let _kernel_trace_service = fuchsia_async::Task::spawn(traces::kernel::serve_forever(
        traces::watcher::subscribe(),
        kernel_stats.clone(),
    ));

    // Serves Fuchsia component inspection protocol
    // https://fuchsia.dev/fuchsia-src/development/diagnostics/inspect
    let _inspect_nodes_service = inspect_nodes::start_service(kernel_stats.clone())?;

    service_fs
        .for_each_concurrent(None, |stream| async {
            match stream {
                Service::MemoryMonitor(stream) => {
                    if let Err(error) =
                        serve_client_stream(stream, &kernel_stats, attribution_client.clone()).await
                    {
                        warn!(%error);
                    }
                }
            }
        })
        .await;

    Ok(())
}

async fn serve_client_stream(
    mut stream: fattribution_plugin::MemoryMonitorRequestStream,
    kernel_stats: &fidl_fuchsia_kernel::StatsProxy,
    attribution_client: Arc<attribution_client::AttributionClient>,
) -> Result<(), Error> {
    // Connect to root job
    let root_job = Box::new(
        connect_to_protocol::<fkernel::RootJobForInspectMarker>()
            .context("error connecting to the root job")?
            .get()
            .await?,
    ) as Box<dyn resources::Job>;

    while let Some(request) = stream.next().await.transpose()? {
        match request {
            fattribution_plugin::MemoryMonitorRequest::GetSnapshot { snapshot, control_handle } => {
                if let Err(err) =
                    provide_snapshot(&attribution_client, &root_job, &kernel_stats, snapshot).await
                {
                    // Errors from `serve_snapshot` are all internal errors, not client-induced.
                    error!(%err);
                    control_handle.shutdown_with_epitaph(zx::Status::INTERNAL);
                }
            }
            fattribution_plugin::MemoryMonitorRequest::_UnknownMethod { .. } => {
                stream.control_handle().shutdown_with_epitaph(zx::Status::NOT_SUPPORTED);
            }
        }
    }
    Ok(())
}

/// Constructs a [Snapshot] and sends it, serialized, through the `snapshot` socket.
async fn provide_snapshot(
    attribution_client: &Arc<attribution_client::AttributionClient>,
    root_job: &Box<dyn resources::Job>,
    kernel_stats: &fkernel::StatsProxy,
    snapshot: zx::Socket,
) -> Result<(), Error> {
    duration!(CATEGORY_MEMORY_CAPTURE, c"provide_snapshot");
    let attribution_state = attribution_client.get_attributions();
    let kernel_resources =
        resources::KernelResources::get_resources(&root_job, &attribution_state)?;

    let memory_stats = kernel_stats.get_memory_stats().await?;
    let compression_stats = kernel_stats.get_memory_stats_compression().await?;

    let attribution_snapshot = AttributionSnapshot::new(
        attribution_state,
        kernel_resources,
        memory_stats,
        compression_stats,
    );
    attribution_snapshot.serve(snapshot).await;
    Ok(())
}
