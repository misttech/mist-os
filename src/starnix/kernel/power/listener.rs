// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;

use crate::power::manager::{SuspendResult, SuspendResumeManagerHandle, STARNIX_POWER_ON_LEVEL};
use fidl::endpoints::create_request_stream;
use futures::StreamExt;
use starnix_logging::{log_error, log_info, log_warn};
use {fidl_fuchsia_power_system as fsystem, fuchsia_zircon as zx};

pub(super) fn init_listener(
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
                        log_info!("Resuming from suspend");
                        match power_manager.update_power_level(STARNIX_POWER_ON_LEVEL) {
                            Ok(_) => {
                                // The server is expected to respond once it has performed the
                                // operations required to keep the system awake.
                                if let Err(e) = responder.send() {
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
                    fsystem::ActivityGovernorListenerRequest::OnSuspend { .. } => {
                        log_info!("Attempting to transition to a low-power state");
                    }
                    fsystem::ActivityGovernorListenerRequest::OnSuspendFail { responder } => {
                        log_warn!("Failed to suspend");

                        // We failed to suspend so bring us back to the power on level.
                        match power_manager.update_power_level(STARNIX_POWER_ON_LEVEL) {
                            Ok(()) => {}
                            // What can we really do here?
                            Err(e) => log_error!(
                                "Failed to create a power-on lease after suspend failure: {e}"
                            ),
                        }

                        // Wake up a potentially blocked suspend.
                        power_manager.notify_suspension(SuspendResult::Failure);

                        if let Err(e) = responder.send() {
                            log_error!("Failed to send OnSuspendFail response: {e}");
                        }
                    }
                    fsystem::ActivityGovernorListenerRequest::_UnknownMethod {
                        ordinal, ..
                    } => {
                        log_error!("Got unexpected method: {}", ordinal)
                    }
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
