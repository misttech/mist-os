// Copyright 2023 The Fuchsia Authors. All rights reserved>.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.cti

use {
    crate::model::{
        actions::StopAction,
        component::{StartReason, WeakComponentInstance},
    },
    fidl::endpoints::RequestStream,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_runner as fcrunner,
    fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::TryStreamExt,
    tracing::warn,
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
    while let Some(request) = stream.try_next().await? {
        match request {
            fcomponent::ControllerRequest::Start { mut args, execution_controller, responder } => {
                let component = weak_component_instance.upgrade();
                if component.is_err() {
                    responder.send(Err(fcomponent::Error::InstanceNotFound))?;
                    continue;
                }
                let component = component.unwrap();
                let execution = component.lock_execution().await;
                if execution.runtime.is_some() {
                    responder.send(Err(fcomponent::Error::InstanceAlreadyStarted))?;
                    continue;
                }
                drop(execution);
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
                let numbered_handles = args.numbered_handles.take().unwrap_or_default();
                let namespace_entries = args
                    .namespace_entries
                    .take()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|e| fcrunner::ComponentNamespaceEntry {
                        path: e.path,
                        directory: e.directory,
                        ..Default::default()
                    })
                    .collect::<Vec<_>>();
                if let Err(err) = component
                    .start(
                        &StartReason::Controller,
                        Some(execution_controller),
                        numbered_handles,
                        namespace_entries,
                    )
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
                responder.send(Ok(component.lock_execution().await.runtime.is_some()))?;
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
        let status = self
            .stop_payload
            .as_ref()
            .expect("stop_payload was not set before the ExecutionControllerTask was dropped");
        // There's not much we can do if the other end has closed their channel
        let _ = self.control_handle.send_on_stop(&fcomponent::StoppedPayload {
            status: Some(status.into_raw()),
            ..Default::default()
        });
    }
}

impl ExecutionControllerTask {
    pub fn set_stop_status(&mut self, status: zx::Status) {
        self.stop_payload = Some(status);
    }
}
