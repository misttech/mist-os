// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl::endpoints::{ControlHandle, RequestStream};
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_memory_attribution as fattribution;
use fidl_fuchsia_settings as fsettings;
use fidl_fuchsia_starnix_runner as fstarnixrunner;
use fuchsia_component::{client::connect_to_protocol_sync, server::ServiceFs};
use fuchsia_sync::Mutex;
use fuchsia_zircon as zx;
use fuchsia_zircon::{HandleBased, Task};
use futures::{StreamExt, TryStreamExt};
use kernel_manager::StarnixKernel;
use std::sync::Arc;
use tracing::{info, warn};
use zx::AsHandleRef;

mod kernels;
use crate::kernels::Kernels;

enum Services {
    ComponentRunner(frunner::ComponentRunnerRequestStream),
    StarnixManager(fstarnixrunner::ManagerRequestStream),
    AttributionProvider(fattribution::ProviderRequestStream),
}

#[fuchsia::main(logging_tags = ["starnix_runner"])]
async fn main() -> Result<(), Error> {
    let config = starnix_runner_config::Config::take_from_startup_handle();
    if config.enable_data_collection {
        info!("Attempting to set user data sharing consent.");
        if let Ok(privacy) = connect_to_protocol_sync::<fsettings::PrivacyMarker>() {
            let privacy_settings = fsettings::PrivacySettings {
                user_data_sharing_consent: Some(true),
                ..Default::default()
            };
            match privacy.set(&privacy_settings, zx::Time::INFINITE) {
                Ok(Ok(())) => info!("Successfully set user data sharing consent."),
                Ok(Err(err)) => warn!("Could not set user data sharing consent: {err:?}"),
                Err(err) => warn!("Could not set user data sharing consent: {err:?}"),
            }
        } else {
            warn!("failed to connect to fuchsia.settings.Privacy");
        }
    }

    let kernels = Kernels::new();
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(Services::ComponentRunner);
    fs.dir("svc").add_fidl_service(Services::StarnixManager);
    fs.dir("svc").add_fidl_service(Services::AttributionProvider);
    fs.take_and_serve_directory_handle()?;
    let suspended_processes = Arc::new(Mutex::new(vec![]));
    fs.for_each_concurrent(None, |request: Services| async {
        match request {
            Services::ComponentRunner(stream) => serve_component_runner(stream, &kernels)
                .await
                .expect("failed to start component runner"),
            Services::StarnixManager(stream) => {
                serve_starnix_manager(stream, suspended_processes.clone(), &kernels)
                    .await
                    .expect("failed to serve starnix manager")
            }
            Services::AttributionProvider(stream) => serve_attribution_provider(stream, &kernels)
                .await
                .expect("failed to serve attribution provider"),
        }
    })
    .await;
    Ok(())
}

async fn serve_component_runner(
    mut stream: frunner::ComponentRunnerRequestStream,
    kernels: &Kernels,
) -> Result<(), Error> {
    while let Some(event) = stream.try_next().await? {
        match event {
            frunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                kernels.start(start_info, controller).await?;
            }
        }
    }
    Ok(())
}

async fn serve_starnix_manager(
    mut stream: fstarnixrunner::ManagerRequestStream,
    suspended_processes: Arc<Mutex<Vec<zx::Handle>>>,
    kernels: &Kernels,
) -> Result<(), Error> {
    while let Some(event) = stream.try_next().await? {
        match event {
            fstarnixrunner::ManagerRequest::Suspend { .. } => {
                let kernels = kernels.list();
                for kernel in kernels {
                    suspended_processes.lock().append(&mut suspend_kernel(&kernel).await);
                }
            }
            fstarnixrunner::ManagerRequest::Resume { .. } => {
                // Drop all the suspend handles to resume the kernel.
                *suspended_processes.lock() = vec![];
            }
            _ => {}
        }
    }
    Ok(())
}

async fn serve_attribution_provider(
    mut stream: fattribution::ProviderRequestStream,
    kernels: &Kernels,
) -> Result<(), Error> {
    let observer = kernels.new_memory_attribution_observer(stream.control_handle());
    while let Some(event) = stream.try_next().await? {
        match event {
            fattribution::ProviderRequest::Get { responder } => {
                observer.next(responder);
            }
            fattribution::ProviderRequest::_UnknownMethod { ordinal, control_handle, .. } => {
                tracing::error!("Invalid request to AttributionProvider: {ordinal}");
                control_handle.shutdown_with_epitaph(zx::Status::INVALID_ARGS);
            }
        }
    }
    Ok(())
}

/// Suspends `kernel` by suspending all the processes in the kernel's job.
async fn suspend_kernel(kernel: &StarnixKernel) -> Vec<zx::Handle> {
    let mut handles = std::collections::HashMap::<zx::Koid, zx::Handle>::new();
    loop {
        let process_koids = kernel.job().processes().expect("failed to get processes");
        let mut found_new_process = false;
        let mut processes = vec![];

        for process_koid in process_koids {
            if handles.get(&process_koid).is_some() {
                continue;
            }

            found_new_process = true;

            if let Ok(process_handle) =
                kernel.job().get_child(&process_koid, zx::Rights::SAME_RIGHTS.bits())
            {
                let process = zx::Process::from_handle(process_handle);
                if let Ok(suspend_handle) = process.suspend() {
                    handles.insert(process_koid, suspend_handle);
                }
                processes.push(process);
            }
        }

        for process in processes {
            let threads = process.threads().expect("failed to get threads");
            for thread_koid in &threads {
                if let Ok(thread_handle) =
                    process.get_child(&thread_koid, zx::Rights::SAME_RIGHTS.bits())
                {
                    let thread = zx::Thread::from_handle(thread_handle);
                    match thread.wait_handle(
                        zx::Signals::THREAD_SUSPENDED,
                        zx::Time::after(zx::Duration::INFINITE),
                    ) {
                        Err(e) => tracing::warn!("Error waiting for task suspension: {:?}", e),
                        _ => {}
                    }
                }
            }
        }

        if !found_new_process {
            break;
        }
    }

    handles.into_values().collect()
}
