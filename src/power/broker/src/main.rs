// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Error};
use async_utils::event::Event;
use fidl::endpoints::{create_request_stream, ControlHandle, Responder, ServerEnd};
use fidl_fuchsia_power_broker::{
    self as fpb, ElementControlMarker, ElementControlRequest, ElementControlRequestStream,
    LeaseControlMarker, LeaseControlRequest, LeaseControlRequestStream, LeaseStatus, LessorMarker,
    LessorRequest, LessorRequestStream, LevelControlMarker, LevelControlRequest,
    LevelControlRequestStream, PowerLevel, StatusRequest, StatusRequestStream, TopologyRequest,
    TopologyRequestStream,
};
use fuchsia_async::Task;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{component, health::Reporter};
use futures::channel::mpsc::UnboundedReceiver;
use futures::prelude::*;
use futures::select;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::broker::{Broker, LeaseID};
use crate::topology::ElementID;

mod broker;
mod credentials;
mod topology;

/// Wraps all hosted protocols into a single type that can be matched against
/// and dispatched.
enum IncomingRequest {
    Topology(TopologyRequestStream),
}

struct BrokerSvc {
    broker: Rc<RefCell<Broker>>,
    status_channel_handlers: Rc<RefCell<HashMap<ElementID, Vec<StatusChannelHandler>>>>,
}

impl BrokerSvc {
    fn new() -> Self {
        Self {
            broker: Rc::new(RefCell::new(Broker::new())),
            status_channel_handlers: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    async fn run_lessor(
        self: Rc<Self>,
        element_id: ElementID,
        stream: LessorRequestStream,
    ) -> Result<(), Error> {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async {
                match request {
                    LessorRequest::Lease { level, responder } => {
                        tracing::debug!("Lease({:?}, {:?})", &element_id, &level);
                        let resp = {
                            let mut broker = self.broker.borrow_mut();
                            broker.acquire_lease(&element_id, level)
                        };
                        match resp {
                            Ok(lease) => {
                                tracing::debug!("responder.send({:?})", &lease);
                                let (client, stream) =
                                    create_request_stream::<LeaseControlMarker>()?;
                                tracing::debug!("Spawning lease control task for {:?}", &lease.id);
                                Task::local({
                                    let svc = self.clone();
                                    async move {
                                        if let Err(err) =
                                            svc.run_lease_control(&lease.id, stream).await
                                        {
                                            tracing::debug!("run_lease_control err: {:?}", err);
                                        }
                                        // When the channel is closed, drop the lease.
                                        let mut broker = svc.broker.borrow_mut();
                                        if let Err(err) = broker.drop_lease(&lease.id) {
                                            tracing::error!("Lease: drop_lease failed: {:?}", err);
                                        }
                                    }
                                })
                                .detach();
                                responder.send(Ok(client)).context("send failed")
                            }
                            Err(err) => responder.send(Err(err.into())).context("send failed"),
                        }
                    }
                    LessorRequest::_UnknownMethod { ordinal, .. } => {
                        tracing::warn!("Received unknown LessorRequest: {ordinal}");
                        todo!()
                    }
                }
            })
            .await
    }

    async fn run_lease_control(
        &self,
        lease_id: &LeaseID,
        stream: LeaseControlRequestStream,
    ) -> Result<(), Error> {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async {
                match request {
                    LeaseControlRequest::WatchStatus {
                        last_status,
                        responder,
                    } => {
                        tracing::debug!(
                            "WatchStatus({:?}, {:?})",
                            lease_id,
                            &last_status
                        );
                        let mut receiver = {
                            let mut broker = self.broker.borrow_mut();
                            broker.watch_lease_status(lease_id)
                        };
                        while let Some(next) = receiver.next().await {
                            tracing::debug!(
                                "receiver.next = {:?}, last_status = {:?}",
                                &next,
                                last_status
                            );
                            let status = next.unwrap_or(LeaseStatus::Unknown);
                            if last_status != LeaseStatus::Unknown && last_status == status {
                                tracing::debug!(
                                    "WatchStatus: status has not changed, watching for next update...",
                                );
                                continue;
                            } else {
                                tracing::debug!(
                                    "WatchStatus: sending new status: {:?}", &status,
                                );
                                return responder.send(status).context("send failed");
                            }
                        }
                        Err(anyhow::anyhow!("Receiver closed, element is no longer available."))
                    }
                    LeaseControlRequest::_UnknownMethod { ordinal, .. } => {
                        tracing::warn!("Received unknown LeaseControlRequest: {ordinal}");
                        todo!()
                    }
                }
            })
            .await
    }

    async fn run_element_control(
        self: Rc<Self>,
        element_id: ElementID,
        stream: ElementControlRequestStream,
    ) -> Result<(), Error> {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async {
                match request {
                    ElementControlRequest::OpenStatusChannel { status_channel, .. } => {
                        tracing::debug!("OpenStatus({:?})", &element_id);
                        let svc = self.clone();
                        svc.create_status_channel_handler(element_id.clone(), status_channel).await
                    }
                    ElementControlRequest::RemoveElement { responder } => {
                        tracing::debug!("RemoveElement({:?})", &element_id);
                        let mut broker = self.broker.borrow_mut();
                        broker.remove_element(&element_id);

                        // Clean up StatusChannelHandlers.
                        self.status_channel_handlers.borrow_mut().remove(&element_id);

                        // Close the ElementControl channel.
                        let control_handle = responder.control_handle().clone();
                        let res = responder.send().context("send failed");
                        control_handle.shutdown();
                        res
                    }
                    ElementControlRequest::AddDependency {
                        dependency_type,
                        dependent_level,
                        requires_token,
                        requires_level,
                        responder,
                    } => {
                        tracing::debug!("AddDependency({:?},{:?},{:?},{:?},{:?})", &element_id, dependency_type, &dependent_level, &requires_token, &requires_level);
                        let mut broker = self.broker.borrow_mut();
                        let res = broker.add_dependency(
                            &element_id,
                            dependency_type,
                            dependent_level,
                            requires_token.into(),
                            requires_level,
                        );
                        tracing::debug!("AddDependency add_dependency = ({:?})", &res);
                        if let Err(err) = res {
                            responder.send(Err(err.into())).context("send failed")
                        } else {
                            responder.send(Ok(())).context("send failed")
                        }
                    }
                    ElementControlRequest::RemoveDependency {
                        dependency_type,
                        dependent_level,
                        requires_token,
                        requires_level,
                        responder,
                    } => {
                        tracing::debug!("RemoveDependency({:?},{:?},{:?},{:?},{:?})", &element_id, dependency_type, &dependent_level, &requires_token, &requires_level);
                        let mut broker = self.broker.borrow_mut();
                        let res = broker.remove_dependency(
                            &element_id,
                            dependency_type,
                            dependent_level,
                            requires_token.into(),
                            requires_level,
                        );
                        tracing::debug!("RemoveDependency remove_dependency = ({:?})", &res);
                        if let Err(err) = res {
                            responder.send(Err(err.into())).context("send failed")
                        } else {
                            responder.send(Ok(())).context("send failed")
                        }
                    }
                    ElementControlRequest::RegisterDependencyToken {
                        token,
                        dependency_type,
                        responder,
                    } => {
                        tracing::debug!(
                            "RegisterDependencyToken({:?}, {:?})",
                            &element_id,
                            &token,
                        );
                        let mut broker = self.broker.borrow_mut();
                        let res = broker.register_dependency_token(
                            &element_id,
                            token.into(),
                            dependency_type,
                        );
                        tracing::debug!("RegisterDependencyToken register_credentials = ({:?})", &res);
                        if let Err(err) = res {
                            responder.send(Err(err.into())).context("send failed")
                        } else {
                            responder.send(Ok(())).context("send failed")
                        }
                    }
                    ElementControlRequest::UnregisterDependencyToken {
                        token,
                        responder,
                    } => {
                        tracing::debug!(
                            "UnregisterDependencyToken({:?}, {:?})",
                            &element_id,
                            &token,
                        );
                        let mut broker = self.broker.borrow_mut();
                        let res = broker.unregister_dependency_token(
                            &element_id,
                            token.into(),
                        );
                        tracing::debug!("UnregisterDependencyToken unregister_credentials = ({:?})", &res);
                        if let Err(err) = res {
                            responder.send(Err(err.into())).context("send failed")
                        } else {
                            responder.send(Ok(())).context("send failed")
                        }
                    }
                    ElementControlRequest::_UnknownMethod { ordinal, .. } => {
                        tracing::warn!("Received unknown ElementControlRequest: {ordinal}");
                        todo!()
                    }
                }
            })
            .await
    }

    async fn run_level_control(
        &self,
        element_id: ElementID,
        stream: LevelControlRequestStream,
    ) -> Result<(), Error> {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async {
                match request {
                    LevelControlRequest::GetRequiredLevel { responder } => {
                        tracing::debug!("GetRequiredLevel({:?})", &element_id);
                        let required_level = {
                            let mut broker = self.broker.borrow_mut();
                            tracing::debug!("get_required_level({:?})", &element_id);
                            broker.get_required_level(&element_id)
                        };
                        if let Some(required_level) = required_level {
                            responder.send(Ok(required_level)).context("response failed")
                        } else {
                            responder.send(Err(fpb::RequiredLevelError::Unknown)).context("response failed")
                        }
                    }
                    LevelControlRequest::WatchRequiredLevel {
                        last_required_level,
                        responder,
                    } => {
                        tracing::debug!(
                            "WatchRequiredLevel({:?}, {:?})",
                            &element_id,
                            &last_required_level
                        );
                        let mut receiver = {
                            let mut broker = self.broker.borrow_mut();
                            broker.watch_required_level(&element_id)
                        };
                        while let Some(next) = receiver.next().await {
                            tracing::debug!(
                                "receiver.next = {:?}, last_required_level = {:?}",
                                &next,
                                last_required_level
                            );
                            let Some(required_level) = next else {
                                tracing::error!("element missing default required level");
                                return responder.send(Err(fpb::RequiredLevelError::Internal)).context("send failed");
                            };
                            if last_required_level != required_level {
                                tracing::debug!(
                                    "WatchRequiredLevel: sending new level: {:?}", &required_level,
                                );
                                return responder.send(Ok(required_level)).context("send failed");
                            }
                            tracing::debug!(
                                "WatchRequiredLevel: level has not changed, watching for next update...",
                            );
                        }
                        Err(anyhow::anyhow!("Receiver closed, element is no longer available."))
                    }
                    LevelControlRequest::UpdateCurrentPowerLevel {
                        current_level,
                        responder,
                    } => {
                        tracing::debug!(
                            "UpdateCurrentPowerLevel({:?}, {:?})",
                            &element_id,
                            &current_level
                        );
                        let mut broker = self.broker.borrow_mut();
                        let res = broker.update_current_level(&element_id, current_level);
                        match res {
                            Ok(_) => {
                                responder.send(Ok(())).context("send failed")
                            },
                            Err(err) => {
                                tracing::debug!("UpdateCurrentPowerLevel Err: {:?}", &err);
                                responder.send(Err(err.into())).context("send failed")
                            },
                        }
                    }
                    LevelControlRequest::_UnknownMethod { ordinal, .. } => {
                        tracing::warn!("Received unknown LevelControlRequest: {ordinal}");
                        todo!()
                    }
                }
            })
            .await
    }

    async fn run_topology(self: Rc<Self>, stream: TopologyRequestStream) -> Result<(), Error> {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async {
                match request {
                    TopologyRequest::AddElement {
                        element_name,
                        initial_current_level,
                        valid_levels,
                        dependencies,
                        active_dependency_tokens_to_register,
                        passive_dependency_tokens_to_register,
                        responder,
                    } => {
                        tracing::debug!(
                            "AddElement({:?}, {:?}, {:?}, {:?}, {:?}, {:?})",
                            &element_name,
                            &initial_current_level,
                            &valid_levels,
                            &dependencies,
                            &active_dependency_tokens_to_register,
                            &passive_dependency_tokens_to_register,
                        );
                        let mut broker = self.broker.borrow_mut();
                        let active_dependency_tokens = active_dependency_tokens_to_register
                            .into_iter()
                            .map(|d| d.into())
                            .collect();
                        let passive_dependency_tokens = passive_dependency_tokens_to_register
                            .into_iter()
                            .map(|d| d.into())
                            .collect();
                        let res = broker.add_element(
                            &element_name,
                            initial_current_level,
                            valid_levels,
                            dependencies,
                            active_dependency_tokens,
                            passive_dependency_tokens,
                        );
                        tracing::debug!("AddElement add_element = {:?}", res);
                        match res {
                            Ok(element_id) => {
                                let (element_control_client, element_control_stream) =
                                    create_request_stream::<ElementControlMarker>()?;
                                tracing::debug!(
                                    "Spawning element control task for {:?}",
                                    &element_id
                                );
                                Task::local({
                                    let svc = self.clone();
                                    let element_id = element_id.clone();
                                    async move {
                                        if let Err(err) = svc
                                            .run_element_control(element_id, element_control_stream)
                                            .await
                                        {
                                            tracing::debug!("run_element_control err: {:?}", err);
                                        }
                                    }
                                })
                                .detach();
                                tracing::debug!("Spawning lessor task for {:?}", &element_id);
                                let (lessor_client, lessor_stream) =
                                    create_request_stream::<LessorMarker>()?;
                                Task::local({
                                    let svc = self.clone();
                                    let element_id = element_id.clone();
                                    async move {
                                        if let Err(err) =
                                            svc.run_lessor(element_id, lessor_stream).await
                                        {
                                            tracing::debug!("run_lessor err: {:?}", err);
                                        }
                                    }
                                })
                                .detach();
                                tracing::debug!(
                                    "Spawning level control task for {:?}",
                                    &element_id
                                );
                                let (level_control_client, level_control_stream) =
                                    create_request_stream::<LevelControlMarker>()?;
                                Task::local({
                                    let svc = self.clone();
                                    let element_id = element_id.clone();
                                    async move {
                                        if let Err(err) = svc
                                            .run_level_control(element_id, level_control_stream)
                                            .await
                                        {
                                            tracing::debug!("run_level_control err: {:?}", err);
                                        }
                                    }
                                })
                                .detach();
                                responder
                                    .send(Ok((
                                        element_control_client,
                                        lessor_client,
                                        level_control_client,
                                    )))
                                    .context("send failed")
                            }
                            Err(err) => responder.send(Err(err.into())).context("send failed"),
                        }
                    }
                    TopologyRequest::_UnknownMethod { ordinal, .. } => {
                        tracing::warn!("Received unknown TopologyRequest: {ordinal}");
                        todo!()
                    }
                }
            })
            .await
    }

    async fn create_status_channel_handler(
        self: Rc<Self>,
        element_id: ElementID,
        server_end: ServerEnd<fpb::StatusMarker>,
    ) -> Result<(), Error> {
        let receiver = {
            let mut broker = self.broker.borrow_mut();
            broker.watch_current_level(&element_id)
        };
        let mut handler = StatusChannelHandler::new(element_id.clone());
        let stream = server_end.into_stream()?;
        handler.start(stream, receiver)?;
        self.status_channel_handlers
            .borrow_mut()
            .entry(element_id.clone())
            .or_insert(Vec::new())
            .push(handler);
        Ok(())
    }
}

struct StatusChannelHandler {
    element_id: ElementID,
    shutdown: Event,
}

impl StatusChannelHandler {
    fn new(element_id: ElementID) -> Self {
        Self { element_id, shutdown: Event::new() }
    }

    fn start(
        &mut self,
        mut stream: StatusRequestStream,
        mut receiver: UnboundedReceiver<Option<PowerLevel>>,
    ) -> Result<(), Error> {
        let element_id = self.element_id.clone();
        let mut shutdown = self.shutdown.wait_or_dropped();
        tracing::debug!("Starting new StatusChannelHandler for {:?}", &self.element_id);
        Task::local(async move {
            loop {
                select! {
                    _ = shutdown => {
                        break;
                    }
                    next = stream.next() => {
                        if let Some(Ok(request)) = next {
                            if let Err(err) = StatusChannelHandler::handle_request(element_id.clone(), request, &mut receiver).await {
                                tracing::debug!("handle_request error: {:?}", err);
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
            tracing::debug!("Closed StatusChannel for {:?}.", &element_id);
        }).detach();
        Ok(())
    }

    async fn handle_request(
        element_id: ElementID,
        request: StatusRequest,
        receiver: &mut UnboundedReceiver<Option<PowerLevel>>,
    ) -> Result<(), Error> {
        match request {
            StatusRequest::WatchPowerLevel { responder } => {
                if let Some(Some(power_level)) = receiver.next().await {
                    tracing::debug!("WatchPowerLevel: send({:?})", &power_level);
                    return responder.send(Ok(power_level)).context("response failed");
                } else {
                    tracing::info!(
                        "WatchPowerLevel: receiver closed, element {:?} is no longer available.",
                        &element_id
                    );
                    return Ok(());
                }
            }
            StatusRequest::_UnknownMethod { ordinal, .. } => {
                tracing::warn!("Received unknown StatusRequest: {ordinal}");
                todo!()
            }
        }
    }
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<(), anyhow::Error> {
    let mut service_fs = ServiceFs::new_local();

    // Initialize inspect
    let _inspect_server = inspect_runtime::publish(
        component::inspector(),
        inspect_runtime::PublishOptions::default(),
    );
    component::health().set_starting_up();

    service_fs.dir("svc").add_fidl_service(IncomingRequest::Topology);

    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;

    component::health().set_ok();

    let svc = Rc::new(BrokerSvc::new());

    service_fs
        .for_each_concurrent(None, |request: IncomingRequest| async {
            match request {
                IncomingRequest::Topology(stream) => {
                    svc.clone().run_topology(stream).await.expect("run_topology failed");
                }
            }
            ()
        })
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[fuchsia::test]
    async fn smoke_test() {
        assert!(true);
    }
}
