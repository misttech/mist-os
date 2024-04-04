// Copyright 2023 The Fuchsia Authors. All rights reserved>.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.cti

use {
    crate::model::{
        actions::StopAction,
        component::{IncomingCapabilities, StartReason, WeakComponentInstance},
    },
    fidl::endpoints::RequestStream,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_sandbox as fsandbox,
    fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::prelude::*,
    sandbox::Capability,
    tracing::{error, warn},
};

pub async fn run_controller(
    weak_component_instance: WeakComponentInstance,
    stream: fcomponent::ControllerRequestStream,
) {
    if let Err(err) = serve_controller(weak_component_instance, stream).await {
        warn!(%err, "failed to serve controller");
    }
}

pub async fn serve_controller(
    weak_component_instance: WeakComponentInstance,
    mut stream: fcomponent::ControllerRequestStream,
) -> Result<(), fidl::Error> {
    let mut tasks = fasync::TaskGroup::new();
    while let Some(request) = stream.try_next().await? {
        match request {
            fcomponent::ControllerRequest::Start { mut args, execution_controller, responder } => {
                let component = weak_component_instance.upgrade();
                let Ok(component) = component else {
                    responder.send(Err(fcomponent::Error::InstanceNotFound))?;
                    continue;
                };
                if component.is_started().await {
                    responder.send(Err(fcomponent::Error::InstanceAlreadyStarted))?;
                    continue;
                }
                let execution_controller_stream = execution_controller.into_stream()?;
                let control_handle = execution_controller_stream.control_handle();
                let execution_controller = ExecutionControllerTask {
                    _task: fasync::Task::spawn(execution_controller_task(
                        weak_component_instance.clone(),
                        execution_controller_stream,
                    )),
                    control_handle,
                    stop_payload: None,
                };

                let incoming = {
                    let numbered_handles = args.numbered_handles.take().unwrap_or_default();

                    let Ok(namespace): Result<namespace::Namespace, _> =
                        args.namespace_entries.take().unwrap_or_default().try_into()
                    else {
                        responder.send(Err(fcomponent::Error::InvalidArguments))?;
                        continue;
                    };

                    let dict = if let Some(dict_client_end) = args.dictionary {
                        let fidl_capability = fsandbox::Capability::Dictionary(dict_client_end);
                        let Ok(any) = Capability::try_from(fidl_capability) else {
                            responder.send(Err(fcomponent::Error::InvalidArguments))?;
                            continue;
                        };
                        let dict = match any {
                            Capability::Dictionary(d) => d,
                            _ => {
                                responder.send(Err(fcomponent::Error::InvalidArguments))?;
                                continue;
                            }
                        };
                        Some(dict)
                    } else {
                        None
                    };

                    IncomingCapabilities {
                        numbered_handles,
                        additional_namespace_entries: namespace.into(),
                        dict,
                    }
                };

                if let Err(err) = component
                    .start(&StartReason::Controller, Some(execution_controller), incoming)
                    .await
                {
                    warn!(%err, "failed to start component");
                    responder.send(Err(err.into()))?;
                    continue;
                }
                responder.send(Ok(()))?;
            }
            fcomponent::ControllerRequest::IsStarted { responder } => {
                let component = weak_component_instance.upgrade();
                if component.is_err() {
                    responder.send(Err(fcomponent::Error::InstanceNotFound))?;
                    continue;
                }
                let component = component.unwrap();
                responder.send(Ok(component.is_started().await))?;
            }
            fcomponent::ControllerRequest::GetExposedDictionary { dictionary, responder } => {
                let res = async {
                    let component = weak_component_instance
                        .upgrade()
                        .map_err(|_| fcomponent::Error::InstanceNotFound)?;
                    let resolved = component
                        .lock_resolved_state()
                        .await
                        .map_err(|_| fcomponent::Error::InstanceCannotResolve)?;
                    let mut exposed_dict = resolved.make_exposed_dict().await;
                    tasks.spawn(async move {
                        if let Err(err) =
                            exposed_dict.serve_dict(dictionary.into_stream().unwrap()).await
                        {
                            warn!(%err, "failed to serve dict");
                        }
                    });
                    Ok(())
                }
                .await;
                responder.send(res)?;
            }
            fcomponent::ControllerRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "fuchsia.component/Controller received unknown method");
            }
        }
    }
    Ok(())
}

async fn execution_controller_task(
    weak_component_instance: WeakComponentInstance,
    mut stream: fcomponent::ExecutionControllerRequestStream,
) {
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fcomponent::ExecutionControllerRequest::Stop { control_handle: _ } => {
                let component = weak_component_instance.upgrade();
                if component.is_err() {
                    return;
                }
                let component = component.unwrap();
                let mut action_set = component.lock_actions().await;
                let _ = action_set.register_no_wait(&component, StopAction::new(false));
            }
            fcomponent::ExecutionControllerRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "fuchsia.component/ExecutionController received unknown method");
            }
        }
    }
}

pub struct ExecutionControllerTask {
    _task: fasync::Task<()>,
    control_handle: fcomponent::ExecutionControllerControlHandle,
    stop_payload: Option<zx::Status>,
}

impl Drop for ExecutionControllerTask {
    fn drop(&mut self) {
        match self.stop_payload.as_ref() {
            Some(status) => {
                // There's not much we can do if the other end has closed their channel
                let _ = self.control_handle.send_on_stop(&fcomponent::StoppedPayload {
                    status: Some(status.into_raw()),
                    ..Default::default()
                });
            }
            None => {
                // TODO(https://fxbug.dev/42081036): stop_payload is not when system is shutting down
                error!("stop_payload was not set before the ExecutionControllerTask was dropped");
            }
        }
    }
}

impl ExecutionControllerTask {
    pub fn set_stop_status(&mut self, status: zx::Status) {
        self.stop_payload = Some(status);
    }
}
