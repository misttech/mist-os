// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Error};
use fidl_fuchsia_power_broker::{
    BinaryPowerLevel, LessorRequest, LessorRequestStream, LevelControlRequest,
    LevelControlRequestStream, PowerLevel, StatusRequest, StatusRequestStream, TopologyRequest,
    TopologyRequestStream,
};
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{component, health::Reporter};
use futures::prelude::*;
use std::sync::{Arc, Mutex};

use crate::broker::Broker;

mod broker;
mod topology;

/// Wraps all hosted protocols into a single type that can be matched against
/// and dispatched.
enum IncomingRequest {
    Lessor(LessorRequestStream),
    LevelControl(LevelControlRequestStream),
    Status(StatusRequestStream),
    Topology(TopologyRequestStream),
}

struct BrokerSvc {
    broker: Arc<Mutex<Broker>>,
}

impl BrokerSvc {
    async fn run_status(&self, stream: StatusRequestStream) -> Result<(), Error> {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async move {
                match request {
                    StatusRequest::GetPowerLevel { element, responder } => {
                        tracing::debug!("GetPowerLevel({:?})", &element);
                        let broker: std::sync::MutexGuard<'_, Broker> = self.broker.lock().unwrap();
                        let result = broker.get_current_level(&element.clone().into());
                        tracing::debug!("get_current_level({:?}) = {:?}", &element, &result);
                        if let Ok(power_level) = result {
                            tracing::debug!("GetPowerLevel responder.send({:?})", &power_level);
                            responder.send(Ok(&power_level)).context("response failed")
                        } else {
                            tracing::debug!("GetPowerLevel responder.send err({:?})", &result);
                            responder.send(Err(result.err().unwrap())).context("response failed")
                        }
                    }
                    StatusRequest::WatchPowerLevel { element, last_level, responder } => {
                        tracing::debug!("WatchPowerLevel({:?}, {:?})", &element, last_level);
                        let mut receiver = {
                            let mut broker: std::sync::MutexGuard<'_, Broker> =
                                self.broker.lock().unwrap();
                            tracing::debug!("subscribe_current_level({:?})", &element);
                            broker.subscribe_current_level(&element.into())
                        };
                        while let Some(Some(power_level)) = receiver.next().await {
                            tracing::debug!(
                                "receiver.next() = {:?} last_level = {:?}",
                                &power_level,
                                &last_level
                            );
                            if power_level != last_level {
                                tracing::debug!("responder.send({:?})", &power_level);
                                return responder.send(Ok(&power_level)).context("response failed");
                            }
                        }
                        Err(anyhow::anyhow!("Not found."))
                    }
                    StatusRequest::_UnknownMethod { .. } => todo!(),
                }
            })
            .await
    }

    async fn run_lessor(&self, stream: LessorRequestStream) -> Result<(), Error> {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async move {
                match request {
                    LessorRequest::Lease { element, level, responder } => {
                        tracing::debug!("Lease({:?}, {:?})", &element, &level);
                        let mut broker = self.broker.lock().unwrap();
                        let resp = broker.acquire_lease(&element.into(), &level);
                        let lease = resp.expect("acquire_lease failed");
                        tracing::debug!("responder.send({:?})", &lease);
                        responder.send(&lease.id).context("send failed")
                    }
                    LessorRequest::DropLease { lease_id, .. } => {
                        tracing::debug!("DropLease({:?})", &lease_id);
                        let mut broker = self.broker.lock().unwrap();
                        broker.drop_lease(&lease_id.into()).expect("drop_lease failed");
                        Ok(())
                    }
                    LessorRequest::_UnknownMethod { .. } => todo!(),
                }
            })
            .await
    }

    async fn run_level_control(&self, stream: LevelControlRequestStream) -> Result<(), Error> {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async move {
                match request {
                    LevelControlRequest::WatchRequiredLevel {
                        element,
                        last_required_level,
                        responder,
                    } => {
                        tracing::debug!(
                            "WatchRequiredLevel({:?}, {:?})",
                            &element,
                            &last_required_level
                        );
                        let mut receiver = {
                            let mut broker: std::sync::MutexGuard<'_, Broker> =
                                self.broker.lock().unwrap();
                            broker.subscribe_required_level(&element.clone().into())
                        };
                        while let Some(next) = receiver.next().await {
                            tracing::debug!(
                                "receiver.next({:?}) = {:?}, last_required_level = {:?}",
                                &element,
                                &next,
                                last_required_level
                            );
                            // TODO(b/299637587): support other power level types.
                            let required_level = next.unwrap_or(PowerLevel::Binary(BinaryPowerLevel::Off));
                            if last_required_level.is_some() && last_required_level.clone().unwrap().as_ref() == &required_level {
                                tracing::debug!(
                                    "WatchRequiredLevel({:?}), level has not changed, watching for next update...",
                                    &element,
                                );
                                continue;
                            } else {
                                tracing::debug!(
                                    "WatchRequiredLevel({:?}), sending new level: {:?}",
                                    &element,
                                    &required_level,
                                );
                                return responder.send(&required_level).context("send failed");
                            }
                        }
                        Err(anyhow::anyhow!("receiver closed unexpectedly"))
                    }
                    LevelControlRequest::UpdateCurrentPowerLevel {
                        element,
                        current_level,
                        responder,
                    } => {
                        tracing::debug!(
                            "UpdateCurrentPowerLevel({:?}, {:?})",
                            &element,
                            &current_level
                        );
                        let mut broker: std::sync::MutexGuard<'_, Broker> =
                            self.broker.lock().unwrap();
                        broker.update_current_level(&element.clone().into(), &current_level);
                        tracing::debug!("UpdateCurrentPowerLevel: responder.send(Ok(()))");
                        return responder.send(Ok(())).context("send failed");
                    }
                    LevelControlRequest::_UnknownMethod { .. } => todo!(),
                }
            })
            .await
    }

    async fn run_topology(&self, stream: TopologyRequestStream) -> Result<(), Error> {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async move {
                match request {
                    TopologyRequest::AddElement { element_name, dependencies, responder } => {
                        tracing::debug!("AddElement({:?}, {:?})", &element_name, &dependencies);
                        let mut broker: std::sync::MutexGuard<'_, Broker> =
                            self.broker.lock().unwrap();
                        let deps = dependencies.into_iter().map(|d| d.into()).collect();
                        let element_id = broker.add_element(&element_name, deps);
                        let element_id_str: String = element_id.into();
                        tracing::debug!("AddElement responder.send({:?})", &element_id_str);
                        responder.send(&element_id_str).context("send failed")
                    }
                    TopologyRequest::RemoveElement { element, responder } => {
                        tracing::debug!("RemoveElement({:?})", &element);
                        let mut broker: std::sync::MutexGuard<'_, Broker> =
                            self.broker.lock().unwrap();
                        let res = broker.remove_element(&element.into());
                        tracing::debug!("RemoveElement remove_element = {:?}", &res);
                        if let Err(err) = res {
                            responder.send(Err(err.into())).context("send failed")
                        } else {
                            responder.send(Ok(())).context("send failed")
                        }
                    }
                    TopologyRequest::AddDependency { dependency, responder } => {
                        tracing::debug!("AddDependency({:?})", &dependency);
                        let mut broker: std::sync::MutexGuard<'_, Broker> =
                            self.broker.lock().unwrap();
                        let res = broker.add_dependency(&dependency.into());
                        tracing::debug!("AddDependency add_dependency = ({:?})", &res);
                        if let Err(err) = res {
                            responder.send(Err(err.into())).context("send failed")
                        } else {
                            responder.send(Ok(())).context("send failed")
                        }
                    }
                    TopologyRequest::RemoveDependency { dependency, responder } => {
                        tracing::debug!("RemoveDependency({:?})", &dependency);
                        let mut broker: std::sync::MutexGuard<'_, Broker> =
                            self.broker.lock().unwrap();
                        let res = broker.remove_dependency(&dependency.into());
                        tracing::debug!("RemoveDependency remove_dependency = ({:?})", &res);
                        if let Err(err) = res {
                            responder.send(Err(err.into())).context("send failed")
                        } else {
                            responder.send(Ok(())).context("send failed")
                        }
                    }
                    TopologyRequest::_UnknownMethod { .. } => todo!(),
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

    // Add services here. E.g:
    // ```
    // service_fs.dir("svc").add_fidl_service(IncomingRequest::MyProtocol);
    // ```
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Lessor);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::LevelControl);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Status);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Topology);

    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;

    component::health().set_ok();

    let svc: BrokerSvc = BrokerSvc { broker: Arc::new(Mutex::new(Broker::new())) };

    service_fs
        .for_each_concurrent(None, |request: IncomingRequest| async {
            match request {
                IncomingRequest::Lessor(stream) => {
                    svc.run_lessor(stream).await.expect("run_lessor failed");
                }
                IncomingRequest::LevelControl(stream) => {
                    svc.run_level_control(stream).await.expect("run_level_control failed");
                }
                IncomingRequest::Status(stream) => {
                    svc.run_status(stream).await.expect("run_status failed");
                }
                IncomingRequest::Topology(stream) => {
                    svc.run_topology(stream).await.expect("run_topology failed");
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
