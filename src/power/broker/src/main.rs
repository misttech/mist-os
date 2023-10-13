// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{self, Context, Error};
use fidl_fuchsia_power_broker::{
    BinaryPowerLevel, LessorRequest, LessorRequestStream, LevelControlRequest,
    LevelControlRequestStream, PowerLevel, StatusRequest, StatusRequestStream,
};
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{component, health::Reporter};
use futures::prelude::*;
use std::{
    sync::{Arc, Mutex},
    thread, time,
};

use crate::broker::Broker;
use crate::topology::{Dependency, ElementLevel, Topology};

mod broker;
mod topology;

/// Wraps all hosted protocols into a single type that can be matched against
/// and dispatched.
enum IncomingRequest {
    LevelControl(LevelControlRequestStream),
    Lessor(LessorRequestStream),
    Status(StatusRequestStream),
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
                        let broker: std::sync::MutexGuard<'_, Broker> = self.broker.lock().unwrap();
                        let result = broker.get_current_level(&element.into());
                        if let Ok(power_level) = result {
                            responder.send(Ok(&power_level)).context("response failed")
                        } else {
                            responder.send(Err(result.err().unwrap())).context("response failed")
                        }
                    }
                    StatusRequest::WatchPowerLevel { element, last_level, responder } => {
                        let mut broker: std::sync::MutexGuard<'_, Broker> =
                            self.broker.lock().unwrap();
                        let mut receiver = broker.subscribe_current_level(&element.into());
                        while let Some(Some(power_level)) = receiver.next().await {
                            if power_level != last_level {
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
                        let mut broker = self.broker.lock().unwrap();
                        let resp = broker.acquire_lease(&element.into(), &level);
                        let lease = resp.expect("acquire_lease failed");
                        responder.send(&lease.id).context("send failed")
                    }
                    LessorRequest::DropLease { lease_id, .. } => {
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
                        // TODO(b/299485602): Make this event-driven using Broker::subscribe_required_level()
                        let start: time::Instant = time::Instant::now();
                        // TODO: Running into an issue where FIDL services not being run in parallel
                        // Adding this timeout allows other calls to sneak in. Fix concurrency issue.
                        let timeout = time::Duration::from_millis(5);
                        loop {
                            {
                                let broker: std::sync::MutexGuard<'_, Broker> =
                                    self.broker.lock().unwrap();
                                let required_level =
                                    broker.get_required_level(&element.clone().into());
                                if Some(Box::new(required_level)) != last_required_level
                                    || time::Instant::now().duration_since(start) > timeout
                                {
                                    return responder.send(&required_level).context("send failed");
                                }
                                drop(broker);
                            }
                            thread::sleep(time::Duration::from_millis(1));
                        }
                    }
                    LevelControlRequest::UpdateCurrentPowerLevel {
                        element,
                        current_level,
                        responder,
                    } => {
                        let mut broker: std::sync::MutexGuard<'_, Broker> =
                            self.broker.lock().unwrap();
                        broker.update_current_level(&element.clone().into(), &current_level);
                        return responder.send(Ok(())).context("send failed");
                    }
                    LevelControlRequest::_UnknownMethod { .. } => todo!(),
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
    service_fs.dir("svc").add_fidl_service(IncomingRequest::LevelControl);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Lessor);
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Status);

    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;

    component::health().set_ok();

    // TODO(b/299463665): Remove hard-coded topology once we have protocols
    // for specifying the topology
    // A <- B <- C -> D
    let mut topology = Topology::new();
    let ba = Dependency {
        level: ElementLevel {
            element: "B".into(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        },
        requires: ElementLevel {
            element: "A".into(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        },
    };
    topology.add_direct_dep(&ba);
    let cb = Dependency {
        level: ElementLevel {
            element: "C".into(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        },
        requires: ElementLevel {
            element: "B".into(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        },
    };
    topology.add_direct_dep(&cb);
    let cd = Dependency {
        level: ElementLevel {
            element: "C".into(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        },
        requires: ElementLevel {
            element: "D".into(),
            level: PowerLevel::Binary(BinaryPowerLevel::On),
        },
    };
    topology.add_direct_dep(&cd);

    let svc: BrokerSvc = BrokerSvc { broker: Arc::new(Mutex::new(Broker::new(topology))) };

    service_fs
        .for_each_concurrent(None, |request: IncomingRequest| async {
            match request {
                IncomingRequest::LevelControl(stream) => {
                    let _ = svc.run_level_control(stream).await;
                }
                IncomingRequest::Lessor(stream) => {
                    let _ = svc.run_lessor(stream).await;
                }
                IncomingRequest::Status(stream) => {
                    let _ = svc.run_status(stream).await;
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
