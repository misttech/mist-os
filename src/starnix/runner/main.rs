// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl::endpoints::{ControlHandle, RequestStream};
use fuchsia_component::client::connect_to_protocol_sync;
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt, TryStreamExt};
use kernel_manager::kernels::Kernels;
use kernel_manager::{serve_starnix_manager, SuspendContext};
use std::sync::Arc;
use tracing::{info, warn};
use {
    fidl_fuchsia_component_runner as frunner, fidl_fuchsia_memory_attribution as fattribution,
    fidl_fuchsia_settings as fsettings, fidl_fuchsia_starnix_runner as fstarnixrunner,
};

enum Services {
    ComponentRunner(frunner::ComponentRunnerRequestStream),
    StarnixManager(fstarnixrunner::ManagerRequestStream),
    AttributionProvider(fattribution::ProviderRequestStream),
}

#[fuchsia::main(logging_tags = ["starnix_runner"])]
async fn main() -> Result<(), Error> {
    fuchsia_trace_provider::trace_provider_create_with_fdio();
    let config = starnix_runner_config::Config::take_from_startup_handle();
    if config.enable_data_collection {
        info!("Attempting to set user data sharing consent.");
        if let Ok(privacy) = connect_to_protocol_sync::<fsettings::PrivacyMarker>() {
            let privacy_settings = fsettings::PrivacySettings {
                user_data_sharing_consent: Some(true),
                ..Default::default()
            };
            match privacy.set(&privacy_settings, zx::MonotonicInstant::INFINITE) {
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
    let suspend_context = Arc::new(SuspendContext::default());
    fs.for_each_concurrent(None, |request: Services| async {
        match request {
            Services::ComponentRunner(stream) => serve_component_runner(stream, &kernels)
                .await
                .expect("failed to start component runner"),
            Services::StarnixManager(stream) => {
                serve_starnix_manager(stream, suspend_context.clone(), &kernels)
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
            frunner::ComponentRunnerRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "Unknown ComponentRunner request");
            }
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
