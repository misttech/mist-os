// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Error};
use fidl_fuchsia_power_broker::{
    self as fpb, LessorRequest, LessorRequestStream, LevelControlMarker, LevelControlRequest,
    LevelControlRequestStream, StatusRequest, StatusRequestStream, TopologyRequest,
    TopologyRequestStream,
};
use fuchsia_async::Task;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{component, health::Reporter};
use futures::lock::Mutex;
use futures::prelude::*;
use std::rc::Rc;

use crate::broker::Broker;
use crate::topology::ElementID;

mod broker;
mod credentials;
mod topology;

/// Wraps all hosted protocols into a single type that can be matched against
/// and dispatched.
enum IncomingRequest {
    Lessor(LessorRequestStream),
    Status(StatusRequestStream),
    Topology(TopologyRequestStream),
}

struct BrokerSvc {
    broker: Rc<Mutex<Broker>>,
}

impl BrokerSvc {
    async fn run_status(&self, stream: StatusRequestStream) -> Result<(), Error> {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each(|request| async {
                match request {
                    StatusRequest::GetPowerLevel { token, responder } => {
                        tracing::debug!("GetPowerLevel({:?})", &token);
                        let broker = self.broker.lock().await;
                        let result = broker.get_current_level(token.into());
                        tracing::debug!("get_current_level = {:?}", &result);
                        if let Ok(power_level) = result {
                            tracing::debug!("GetPowerLevel responder.send({:?})", &power_level);
                            responder.send(Ok(&power_level)).context("response failed")
                        } else {
                            tracing::debug!("GetPowerLevel responder.send err({:?})", &result);
                            responder.send(Err(result.unwrap_err())).context("response failed")
                        }
                    }
                    StatusRequest::WatchPowerLevel { token, last_level, responder } => {
                        tracing::debug!("WatchPowerLevel({:?}, {:?})", &token, last_level);
                        let watch_res = {
                            let mut broker = self.broker.lock().await;
                            tracing::debug!("subscribe_current_level({:?})", &token);
                            broker.watch_current_level(token.into())
                        };
                        let Ok(mut receiver) = watch_res else {
                            return responder
                                .send(Err(watch_res.unwrap_err()))
                                .context("send failed");
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
            .try_for_each(|request| async {
                match request {
                    LessorRequest::Lease { token, level, responder } => {
                        tracing::debug!("Lease({:?}, {:?})", &token, &level);
                        let mut broker = self.broker.lock().await;
                        let resp = broker.acquire_lease(token.into(), level);
                        match resp {
                            Ok(lease) => {
                                tracing::debug!("responder.send({:?})", &lease);
                                responder.send(Ok(&lease.id)).context("send failed")
                            }
                            Err(err) => responder.send(Err(err.into())).context("send failed"),
                        }
                    }
                    LessorRequest::DropLease { lease_id, .. } => {
                        tracing::debug!("DropLease({:?})", &lease_id);
                        let mut broker = self.broker.lock().await;
                        broker.drop_lease(&lease_id.into()).expect("drop_lease failed");
                        Ok(())
                    }
                    LessorRequest::_UnknownMethod { .. } => todo!(),
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
                    LevelControlRequest::WatchRequiredLevel {
                        last_required_level,
                        responder,
                    } => {
                        tracing::debug!(
                            "WatchRequiredLevel({:?}, {:?})",
                            &element_id,
                            &last_required_level
                        );
                        let watch_res = {
                            let mut broker = self.broker.lock().await;
                            broker.watch_required_level(&element_id)
                        };
                        let Ok(mut receiver) = watch_res else {
                            return responder.send(Err(watch_res.unwrap_err())).context("send failed");
                        };
                        while let Some(next) = receiver.next().await {
                            tracing::debug!(
                                "receiver.next = {:?}, last_required_level = {:?}",
                                &next,
                                last_required_level
                            );
                            let Some(required_level) = next else {
                                tracing::error!("element missing default required level");
                                return responder.send(Err(fpb::WatchRequiredLevelError::Internal)).context("send failed");
                            };
                            if last_required_level.is_none() || **last_required_level.as_ref().unwrap() != required_level {
                                tracing::debug!(
                                    "WatchRequiredLevel: sending new level: {:?}", &required_level,
                                );
                                return responder.send(Ok(&required_level)).context("send failed");
                            }
                            tracing::debug!(
                                "WatchRequiredLevel: level has not changed, watching for next update...",
                            );
                        }
                        Err(anyhow::anyhow!("receiver closed unexpectedly"))
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
                    LevelControlRequest::_UnknownMethod { .. } => todo!(),
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
                        default_level,
                        dependencies,
                        credentials_to_register,
                        responder,
                    } => {
                        tracing::debug!(
                            "AddElement({:?}, {:?})",
                            &element_name,
                            &credentials_to_register
                        );
                        let mut broker = self.broker.lock().await;
                        let credentials =
                            credentials_to_register.into_iter().map(|d| d.into()).collect();
                        let res = broker.add_element(
                            &element_name,
                            default_level,
                            dependencies,
                            credentials,
                        );
                        tracing::debug!("AddElement add_element = {:?}", res);
                        match res {
                            Ok(element_id) => {
                                let (client, server) =
                                    fidl::endpoints::create_request_stream::<LevelControlMarker>()?;
                                tracing::debug!(
                                    "Spawning level control task for {:?}",
                                    &element_id
                                );
                                Task::local({
                                    let svc = self.clone();
                                    async move {
                                        if let Err(err) =
                                            svc.run_level_control(element_id, server).await
                                        {
                                            tracing::debug!("run_level_control err: {:?}", err);
                                        }
                                    }
                                })
                                .detach();
                                responder.send(Ok(client)).context("send failed")
                            }
                            Err(err) => responder.send(Err(err.into())).context("send failed"),
                        }
                    }
                    TopologyRequest::RemoveElement { token, responder } => {
                        tracing::debug!("RemoveElement({:?})", &token);
                        let mut broker = self.broker.lock().await;
                        let res = broker.remove_element(token.into());
                        tracing::debug!("RemoveElement remove_element = {:?}", &res);
                        if let Err(err) = res {
                            responder.send(Err(err.into())).context("send failed")
                        } else {
                            responder.send(Ok(())).context("send failed")
                        }
                    }
                    TopologyRequest::AddDependency { dependency, responder } => {
                        tracing::debug!("AddDependency({:?})", &dependency);
                        let mut broker = self.broker.lock().await;
                        let res = broker.add_dependency(
                            dependency.dependent.token.into(),
                            dependency.dependent.level,
                            dependency.requires.token.into(),
                            dependency.requires.level,
                        );
                        tracing::debug!("AddDependency add_dependency = ({:?})", &res);
                        if let Err(err) = res {
                            responder.send(Err(err.into())).context("send failed")
                        } else {
                            responder.send(Ok(())).context("send failed")
                        }
                    }
                    TopologyRequest::RemoveDependency { dependency, responder } => {
                        tracing::debug!("RemoveDependency({:?})", &dependency);
                        let mut broker = self.broker.lock().await;
                        let res = broker.remove_dependency(
                            dependency.dependent.token.into(),
                            dependency.dependent.level,
                            dependency.requires.token.into(),
                            dependency.requires.level,
                        );
                        tracing::debug!("RemoveDependency remove_dependency = ({:?})", &res);
                        if let Err(err) = res {
                            responder.send(Err(err.into())).context("send failed")
                        } else {
                            responder.send(Ok(())).context("send failed")
                        }
                    }
                    TopologyRequest::RegisterCredentials {
                        token,
                        credentials_to_register,
                        responder,
                    } => {
                        tracing::debug!(
                            "RegisterCredentials({:?}, {:?})",
                            &token,
                            &credentials_to_register
                        );
                        let mut broker = self.broker.lock().await;
                        let res = broker.register_credentials(
                            token.into(),
                            credentials_to_register.into_iter().map(|c| c.into()).collect(),
                        );
                        tracing::debug!("RegisterCredentials register_credentials = ({:?})", &res);
                        if let Err(err) = res {
                            responder.send(Err(err.into())).context("send failed")
                        } else {
                            responder.send(Ok(())).context("send failed")
                        }
                    }
                    TopologyRequest::UnregisterCredentials {
                        token,
                        tokens_to_unregister,
                        responder,
                    } => {
                        tracing::debug!(
                            "UnregisterCredentials({:?}, {:?})",
                            &token,
                            &tokens_to_unregister
                        );
                        let mut broker = self.broker.lock().await;
                        let res = broker.unregister_credentials(
                            token.into(),
                            tokens_to_unregister.into_iter().map(|c| c.into()).collect(),
                        );
                        tracing::debug!("RegisterCredentials register_credentials = ({:?})", &res);
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
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Status);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Topology);

    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;

    component::health().set_ok();

    let svc = Rc::new(BrokerSvc { broker: Rc::new(Mutex::new(Broker::new())) });

    service_fs
        .for_each_concurrent(None, |request: IncomingRequest| async {
            match request {
                IncomingRequest::Lessor(stream) => {
                    svc.run_lessor(stream).await.expect("run_lessor failed");
                }
                IncomingRequest::Status(stream) => {
                    svc.run_status(stream).await.expect("run_status failed");
                }
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
