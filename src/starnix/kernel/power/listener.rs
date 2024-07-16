// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;

use crate::power::manager::{SuspendResult, SuspendResumeManagerHandle, STARNIX_POWER_ON_LEVEL};
use fidl::endpoints::create_request_stream;
use futures::StreamExt;
use starnix_logging::{log_error, log_info, log_warn};
use {
    fidl_fuchsia_power_system as fsystem, fidl_fuchsia_session_power as fsession,
    fuchsia_zircon as zx,
};

pub(super) fn init_listener(
    power_manager: &SuspendResumeManagerHandle,
    activity_governor: &fsystem::ActivityGovernorSynchronousProxy,
    system_task: &CurrentTask,
) {
    // TODO(https://fxbug.dev/353575594): have session power management
    // available everywhere, and remove fallback logic.
    log_info!("Initializing power listener");
    if let Err(e) = init_session_listener(power_manager, system_task) {
        log_info!("Connecting to session power management returned {e}");
        log_info!("Session power management unavailable, falling back to system power management");
        init_sag_listener(power_manager, activity_governor, system_task)
    }
}

fn init_session_listener(
    power_manager: &SuspendResumeManagerHandle,
    system_task: &CurrentTask,
) -> Result<(), anyhow::Error> {
    let listener_registry = system_task
        .kernel()
        .connect_to_protocol_at_container_svc::<fsession::ListenerRegistryMarker>()?
        .into_sync_proxy();
    let (listener_client_end, mut listener_stream) =
        create_request_stream::<fsession::BlockingListenerMarker>().unwrap();
    listener_registry.register_blocking_listener(listener_client_end, zx::Time::INFINITE)?;

    let power_manager = power_manager.clone();
    system_task.kernel().kthreads.spawn_future(async move {
        log_info!("Session Power Listener task starting...");

        while let Some(stream) = listener_stream.next().await {
            match stream {
                Ok(req) => match req {
                    fsession::BlockingListenerRequest::OnResumeStarted { responder } => {
                        handle_on_resume(&power_manager, || responder.send())
                    }
                    fsession::BlockingListenerRequest::OnSuspendFailed { responder } => {
                        handle_on_suspend_fail(&power_manager, || responder.send())
                    }
                    fsession::BlockingListenerRequest::_UnknownMethod { ordinal, .. } => {
                        handle_unknown_method(ordinal)
                    }
                },
                Err(e) => {
                    log_error!("listener server got an error: {}", e);
                    break;
                }
            }
        }

        log_warn!("Session Power Listener task done");
    });
    Ok(())
}

fn init_sag_listener(
    power_manager: &SuspendResumeManagerHandle,
    activity_governor: &fsystem::ActivityGovernorSynchronousProxy,
    system_task: &CurrentTask,
) {
    let (listener_client_end, mut listener_stream) =
        create_request_stream::<fsystem::ActivityGovernorListenerMarker>().unwrap();
    let power_manager = power_manager.clone();
    system_task.kernel().kthreads.spawn_future(async move {
        log_info!("Activity Governor Listener task starting...");

        while let Some(stream) = listener_stream.next().await {
            match stream {
                Ok(req) => match req {
                    fsystem::ActivityGovernorListenerRequest::OnResume { responder } => {
                        handle_on_resume(&power_manager, || responder.send())
                    }
                    fsystem::ActivityGovernorListenerRequest::OnSuspend { .. } => {
                        handle_on_suspend()
                    }
                    fsystem::ActivityGovernorListenerRequest::OnSuspendFail { responder } => {
                        handle_on_suspend_fail(&power_manager, || responder.send())
                    }
                    fsystem::ActivityGovernorListenerRequest::_UnknownMethod {
                        ordinal, ..
                    } => handle_unknown_method(ordinal),
                },
                Err(e) => {
                    log_error!("listener server got an error: {}", e);
                    break;
                }
            }
        }

        log_warn!("Activity Governor Listener task done");
    });
    if let Err(err) = activity_governor.register_listener(
        fsystem::ActivityGovernorRegisterListenerRequest {
            listener: Some(listener_client_end),
            ..Default::default()
        },
        zx::Time::INFINITE,
    ) {
        log_error!("failed to register listener in sag {}", err)
    }
}

fn handle_on_resume<F>(power_manager: &SuspendResumeManagerHandle, responder_func: F)
where
    F: FnOnce() -> Result<(), fidl::Error>,
{
    log_info!("Resuming from suspend");
    match power_manager.update_power_level(STARNIX_POWER_ON_LEVEL) {
        Ok(_) => {
            // The server is expected to respond once it has performed the
            // operations required to keep the system awake.
            if let Err(e) = responder_func() {
                log_error!(
                    "OnResume server failed to send a respond to its
                        client: {}",
                    e
                );
            }
        }
        Err(e) => log_error!("Failed to create a power-on lease: {}", e),
    }

    // Wake up a potentially blocked suspend.
    //
    // NB: We can't send this event on the `OnSuspend` listener event
    // since that event is emitted before suspension is actually
    // attempted.
    power_manager.notify_suspension(SuspendResult::Success);
}

fn handle_on_suspend() {
    log_info!("Attempting to transition to a low-power state");
}

fn handle_on_suspend_fail<F>(power_manager: &SuspendResumeManagerHandle, responder_func: F)
where
    F: FnOnce() -> Result<(), fidl::Error>,
{
    log_warn!("Failed to suspend");

    // We failed to suspend so bring us back to the power on level.
    match power_manager.update_power_level(STARNIX_POWER_ON_LEVEL) {
        Ok(()) => {}
        // What can we really do here?
        Err(e) => log_error!("Failed to create a power-on lease after suspend failure: {e}"),
    }

    // Wake up a potentially blocked suspend.
    power_manager.notify_suspension(SuspendResult::Failure);

    if let Err(e) = responder_func() {
        log_error!("Failed to send OnSuspendFail response: {e}");
    }
}

fn handle_unknown_method(ordinal: u64) {
    log_error!("Got unexpected method: {}", ordinal)
}
