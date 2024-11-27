// Copyright 2023 The Fuchsia Authors. All rights reserved>.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::actions::StopAction;
use crate::model::component::{
    ExtendedInstance, IncomingCapabilities, StartReason, WeakComponentInstance,
};
use ::runner::component::StopInfo;
use fidl::endpoints::{ProtocolMarker, RequestStream};
use fuchsia_trace::{self as trace, duration, instant};
use futures::prelude::*;
use tracing::{error, warn};
use {fidl_fuchsia_component as fcomponent, fuchsia_async as fasync};

pub async fn run_controller(
    weak_component_instance: WeakComponentInstance,
    stream: fcomponent::ControllerRequestStream,
) {
    if let Err(err) = serve_controller(weak_component_instance, stream).await {
        warn!(%err, "Error serving {}", fcomponent::ControllerMarker::DEBUG_NAME);
    }
}

pub async fn serve_controller(
    weak_component_instance: WeakComponentInstance,
    mut stream: fcomponent::ControllerRequestStream,
) -> Result<(), fidl::Error> {
    while let Some(request) = stream.try_next().await? {
        match request {
            fcomponent::ControllerRequest::Start { args, execution_controller, responder } => {
                duration!(c"component_manager", c"Controller.Start");
                let res: Result<(), fcomponent::Error> = async {
                    let component = weak_component_instance.upgrade();
                    let Ok(component) = component else {
                        return Err(fcomponent::Error::InstanceNotFound);
                    };
                    if component.is_started().await {
                        return Err(fcomponent::Error::InstanceAlreadyStarted);
                    }
                    let execution_controller_stream = execution_controller.into_stream();
                    let control_handle = execution_controller_stream.control_handle();
                    let execution_controller = ExecutionControllerTask {
                        _task: fasync::Task::spawn(execution_controller_task(
                            weak_component_instance.clone(),
                            execution_controller_stream,
                        )),
                        control_handle,
                        stop_payload: None,
                    };
                    let incoming: IncomingCapabilities = match args.try_into() {
                        Ok(incoming) => incoming,
                        Err(e) => {
                            return Err(e);
                        }
                    };

                    if let Err(err) = component
                        .start(&StartReason::Controller, Some(execution_controller), incoming)
                        .await
                    {
                        warn!(%err, "failed to start component");
                        return Err(err.into());
                    }
                    Ok(())
                }
                .await;
                let result = format!("{res:?}");
                responder.send(res)?;
                instant!(
                    c"component_manager", c"Controller.Start/Result", trace::Scope::Process,
                    "result" => result.as_str());
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
            fcomponent::ControllerRequest::GetExposedDictionary { responder } => {
                let res = async {
                    let component = weak_component_instance
                        .upgrade()
                        .map_err(|_| fcomponent::Error::InstanceNotFound)?;
                    let resolved = component
                        .lock_resolved_state()
                        .await
                        .map_err(|_| fcomponent::Error::InstanceCannotResolve)?;
                    let exposed_dict = resolved.get_exposed_dict().await.clone();
                    Ok(exposed_dict.into())
                }
                .await;
                responder.send(res)?;
            }
            fcomponent::ControllerRequest::Destroy { responder } => {
                let res = (|| {
                    let Ok(component) = weak_component_instance.upgrade() else {
                        return Ok(None);
                    };
                    let Ok(ext_parent) = component.parent.upgrade() else {
                        return Ok(None);
                    };
                    let parent;
                    match ext_parent {
                        ExtendedInstance::AboveRoot(_) => {
                            // This is the root component, which can't be destroyed
                            return Err(fcomponent::Error::AccessDenied);
                        }
                        ExtendedInstance::Component(p) => {
                            parent = p;
                        }
                    }
                    let moniker = component.moniker.clone();
                    if let None = moniker
                        .leaf()
                        .expect("we already checked this is not the root component")
                        .collection()
                    {
                        // Disallow destruction of static children
                        return Err(fcomponent::Error::AccessDenied);
                    }
                    Ok(Some((parent, moniker)))
                })();
                let (parent, moniker) = match res {
                    Ok(Some(v)) => v,
                    Ok(None) => {
                        // Already destroyed.
                        responder.send(Ok(()))?;
                        continue;
                    }
                    Err(e) => {
                        responder.send(Err(e))?;
                        continue;
                    }
                };
                // Be sure to return a response before scheduling the subtask. Otherwise it's possible
                // (even if unlikely) that the subtask completes and cancels this task before the
                // code to send the response executes.
                responder.send(Ok(()))?;
                parent.nonblocking_task_group().spawn(async move {
                    let child_name =
                        moniker.leaf().expect("we already checked this is not the root component");
                    if let Err(err) = parent.remove_dynamic_child(&child_name).await {
                        warn!(%err, %moniker,
                                "Controller/Destroy: component destruction unexpectedly failed");
                    }
                    // If `res` was Ok, the contract of this method states that we should close
                    // the channel and not return a response. That should happen because destroying
                    // this component should cancel this task which owns the channel.
                });
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
                component.unwrap().actions().register_no_wait(StopAction::new(false)).await;
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
    stop_payload: Option<StopInfo>,
}

impl Drop for ExecutionControllerTask {
    fn drop(&mut self) {
        match self.stop_payload.as_ref() {
            Some(payload) => {
                // There's not much we can do if the other end has closed their channel
                let _ = self.control_handle.send_on_stop(&fcomponent::StoppedPayload {
                    status: Some(payload.termination_status.into_raw()),
                    exit_code: payload.exit_code,
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
    pub fn set_stop_payload(&mut self, info: StopInfo) {
        self.stop_payload = Some(info);
    }
}
