// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod cpu_element_manager;
mod cpu_manager;
mod events;
mod system_activity_governor;

use crate::cpu_element_manager::{CpuElementManager, SystemActivityGovernorFactory};
use crate::events::SagEventLogger;
use crate::system_activity_governor::SystemActivityGovernor;
use anyhow::{Context, Result};
use fuchsia_async::{DurationExt, TimeoutExt};
use fuchsia_component::client::{connect_to_protocol, Service};
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::health::Reporter;
use fuchsia_inspect::BoolProperty as IBool;
use futures::{FutureExt, StreamExt};
use sag_config::Config;
use std::rc::Rc;
use std::time::Duration;
use zx::MonotonicDuration;
use {
    fidl_fuchsia_hardware_suspend as fhsuspend, fidl_fuchsia_power_broker as fbroker,
    fidl_fuchsia_power_observability as fobs, fidl_fuchsia_power_suspend as fsuspend,
    fidl_fuchsia_power_system as fsystem,
};

const SUSPEND_DEVICE_TIMEOUT: MonotonicDuration = MonotonicDuration::from_seconds(10);
const SUSPENDER_CONNECT_RETRY_DELAY: Duration = Duration::from_secs(3);

async fn connect_to_suspender() -> Result<fhsuspend::SuspenderProxy> {
    Service::open(fhsuspend::SuspendServiceMarker)?
        .watch_for_any()
        .on_timeout(SUSPEND_DEVICE_TIMEOUT.after_now(), || {
            Err(anyhow::anyhow!("Timeout waiting for next watcher message."))
        })
        .await?
        .connect_to_suspender()
        .map_err(|e| anyhow::anyhow!("Failed to connect to suspender: {:?}", e))
}

enum IncomingService {
    ActivityGovernor(fsystem::ActivityGovernorRequestStream),
    BootControl(fsystem::BootControlRequestStream),
    CpuElementManager(fsystem::CpuElementManagerRequestStream),
    Stats(fsuspend::StatsRequestStream),
    ElementInfoProviderService(fbroker::ElementInfoProviderServiceRequest),
}

async fn run<F>(cpu_service: Rc<CpuElementManager<F>>, booting_node: Rc<IBool>) -> Result<()>
where
    F: SystemActivityGovernorFactory,
{
    let mut service_fs = ServiceFs::new_local();

    service_fs
        .dir("svc")
        .add_fidl_service(IncomingService::ActivityGovernor)
        .add_fidl_service(IncomingService::BootControl)
        .add_fidl_service(IncomingService::Stats)
        .add_fidl_service(IncomingService::CpuElementManager)
        .add_fidl_service_instance(
            "system_activity_governor",
            IncomingService::ElementInfoProviderService,
        );
    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;

    service_fs
        .for_each_concurrent(None, move |request: IncomingService| {
            let cpu_service = cpu_service.clone();
            let booting_node = booting_node.clone();
            // Before constructing the SystemActivityGovernor type, the system-activity-governor
            // component must receive a token from another component. To ensure components that
            // depend on fuchsia.power.system.ActivityGovernor, et. al. have consistent behavior,
            // this component only handles messages from fuchsia.power.system.CpuElementManager
            // until the SystemActivityGovernor type is constructed.
            async move {
                match request {
                    IncomingService::ActivityGovernor(stream) => {
                        cpu_service.sag().await.handle_activity_governor_stream(stream).await
                    }
                    IncomingService::BootControl(stream) => {
                        cpu_service
                            .sag()
                            .await
                            .handle_boot_control_stream(stream, booting_node)
                            .await
                    }
                    IncomingService::CpuElementManager(stream) => {
                        cpu_service.handle_cpu_element_manager_stream(stream).await
                    }
                    IncomingService::Stats(stream) => {
                        cpu_service.sag().await.handle_stats_stream(stream).await
                    }
                    IncomingService::ElementInfoProviderService(
                        fbroker::ElementInfoProviderServiceRequest::StatusProvider(stream),
                    ) => cpu_service.sag().await.handle_element_info_provider_stream(stream).await,
                }
            }
        })
        .await;

    Ok(())
}

#[fuchsia::main]
async fn main() -> Result<()> {
    log::info!("started");
    fuchsia_trace_provider::trace_provider_create_with_fdio();

    let inspector = fuchsia_inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());
    fuchsia_inspect::component::health().set_starting_up();

    let config = Config::take_from_startup_handle();
    inspector.root().record_child("config", |config_node| config.record_inspect(config_node));

    // Set up the SystemActivityGovernor.
    log::info!(config:?; "config");

    let suspender = if config.use_suspender {
        loop {
            log::info!("Attempting to connect to suspender...");
            match connect_to_suspender().await {
                Ok(s) => {
                    log::info!("Connected to suspender");
                    break Some(s);
                }
                Err(e) => {
                    log::error!("Unable to connect to suspender protocol: {e:?}");
                }
            }
            // Delay retry for some time to reduce log spam.
            fuchsia_async::Timer::new(SUSPENDER_CONNECT_RETRY_DELAY).await;
        }
    } else {
        log::info!("Skipping connecting to suspender.");
        None
    };

    let topology = connect_to_protocol::<fbroker::TopologyMarker>()?;
    let sag_event_logger =
        SagEventLogger::new(inspector.root().create_child(fobs::SUSPEND_EVENTS_NODE));

    let topology2 = topology.clone();
    let sag_event_logger2 = sag_event_logger.clone();

    let sag_factory_fn = move |cpu_manager, execution_state_dependencies| {
        let topology = topology2.clone();
        let sag_event_logger = sag_event_logger2.clone();
        async move {
            log::info!("Creating activity governor server...");
            SystemActivityGovernor::new(
                &topology,
                inspector.root().clone_weak(),
                sag_event_logger,
                cpu_manager,
                execution_state_dependencies,
            )
            .await
        }
        .boxed_local()
    };

    let cpu_service = if config.wait_for_suspending_token {
        CpuElementManager::new_wait_for_suspending_token(
            &topology,
            inspector.root().clone_weak(),
            sag_event_logger,
            suspender,
            sag_factory_fn,
        )
        .await
    } else {
        CpuElementManager::new(
            &topology,
            inspector.root().clone_weak(),
            sag_event_logger,
            suspender,
            sag_factory_fn,
        )
        .await
    };

    fuchsia_inspect::component::health().set_ok();

    // This future should never complete.
    let booting_node = Rc::new(inspector.root().create_bool("booting", true));
    let result = run(cpu_service, booting_node).await;
    log::error!(result:?; "Unexpected exit");
    fuchsia_inspect::component::health().set_unhealthy(&format!("Unexpected exit: {:?}", result));
    result
}
