// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Error};
use fidl::endpoints::{create_request_stream, ControlHandle, Responder};
use fidl_fuchsia_power_broker::{
    self as fpb, ElementControlMarker, ElementControlRequest, ElementControlRequestStream,
    LeaseControlMarker, LeaseControlRequest, LeaseControlRequestStream, LeaseStatus, LessorMarker,
    LessorRequest, LessorRequestStream, LevelControlMarker, LevelControlRequest,
    LevelControlRequestStream, StatusRequest, StatusRequestStream, TopologyRequest,
    TopologyRequestStream,
};
use fuchsia_async::Task;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{component, health::Reporter};
use futures::lock::Mutex;
use futures::prelude::*;
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
    broker: Rc<Mutex<Broker>>,
}

impl BrokerSvc {
    async fn run_status(
        self: Rc<Self>,
        element_id: ElementID,
        stream: StatusRequestStream,
    ) -> Result<(), Error> {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async {
                match request {
                    StatusRequest::GetPowerLevel { responder } => {
                        tracing::debug!("GetPowerLevel({:?})", &element_id);
                        let current_level = {
                            let mut broker = self.broker.lock().await;
                            tracing::debug!("get_current_level({:?})", &element_id);
                            broker.get_current_level(&element_id)
                        };
                        if let Some(current_level) = current_level {
                            responder.send(Ok(current_level)).context("response failed")
                        } else {
                            responder.send(Err(fpb::StatusError::Unknown)).context("response failed")
                        }
                    }
                    StatusRequest::WatchPowerLevel { last_level, responder } => {
                        tracing::debug!("WatchPowerLevel({:?}, {:?})", &element_id, last_level);
                        let mut receiver = {
                            let mut broker = self.broker.lock().await;
                            tracing::debug!("subscribe_current_level({:?})", &element_id);
                            broker.watch_current_level(&element_id)
                        };
                        while let Some(Some(power_level)) = receiver.next().await {
                            tracing::debug!(
                                "receiver.next() = {:?} last_level = {:?}",
                                &power_level,
                                &last_level
                            );
                            if last_level != power_level {
                                tracing::debug!("responder.send({:?})", &power_level);
                                return responder.send(Ok(power_level)).context("response failed");
                            }
                            tracing::debug!(
                                "WatchPowerLevel: level has not changed, watching for next update...",
                            );
                        }
                        Err(anyhow::anyhow!("Receiver closed, element is no longer available."))
                    }
                    StatusRequest::_UnknownMethod { ordinal, .. } => {
                        tracing::warn!("Received unknown StatusRequest: {ordinal}");
                        todo!()
                    }
                }
            })
            .await
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
                            let mut broker = self.broker.lock().await;
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
                                        let mut broker = svc.broker.lock().await;
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
                            let mut broker = self.broker.lock().await;
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
                        let Ok(stream) = status_channel.into_stream() else {
                            return Err(anyhow::anyhow!("OpenStatus: into_stream failed"));
                        };
                        tracing::debug!("Spawning new status task for {:?}", &element_id);
                        Task::local({
                            let svc = self.clone();
                            let element_id = element_id.clone();
                            async move {
                                if let Err(err) = svc.run_status(element_id, stream).await {
                                    tracing::debug!("run_status err: {:?}", err);
                                }
                            }
                        })
                        .detach();
                        Ok(())
                    }
                    ElementControlRequest::RemoveElement { responder } => {
                        tracing::debug!("RemoveElement({:?})", &element_id);
                        let mut broker = self.broker.lock().await;
                        broker.remove_element(&element_id);
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
                        let mut broker = self.broker.lock().await;
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
                        let mut broker = self.broker.lock().await;
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
                        let mut broker = self.broker.lock().await;
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
                        let mut broker = self.broker.lock().await;
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
                            let mut broker = self.broker.lock().await;
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
                            let mut broker = self.broker.lock().await;
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
                        let mut broker = self.broker.lock().await;
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
                        minimum_level,
                        dependencies,
                        active_dependency_tokens_to_register,
                        passive_dependency_tokens_to_register,
                        responder,
                    } => {
                        tracing::debug!(
                            "AddElement({:?}, {:?}, {:?}, {:?}, {:?}, {:?})",
                            &element_name,
                            &initial_current_level,
                            &minimum_level,
                            &dependencies,
                            &active_dependency_tokens_to_register,
                            &passive_dependency_tokens_to_register,
                        );
                        let mut broker = self.broker.lock().await;
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
                            minimum_level,
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

    let svc = Rc::new(BrokerSvc { broker: Rc::new(Mutex::new(Broker::new())) });

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
