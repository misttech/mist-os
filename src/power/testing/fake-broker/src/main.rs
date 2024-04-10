// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    rc::Rc,
    sync::{Mutex, MutexGuard},
};

use anyhow::{Error, Result};
use fidl::endpoints::create_request_stream;
use fidl_fuchsia_power_broker::{
    CurrentLevelRequest, CurrentLevelRequestStream, ElementControlMarker, ElementControlRequest,
    ElementControlRequestStream, LeaseControlMarker, LeaseControlRequest,
    LeaseControlRequestStream, LeaseStatus, LessorRequest, LessorRequestStream, PowerLevel,
    StatusRequest, StatusRequestStream, TopologyRequest, TopologyRequestStream,
};
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt, TryStreamExt};
use tracing::info;

enum IncomingRequest {
    Topology(TopologyRequestStream),
}

#[derive(Default)]
struct FakePowerBroker {
    inner: Mutex<FakePowerBrokerInner>,
}

#[derive(Default)]
struct FakePowerBrokerInner {
    current_level: PowerLevel,
}

impl FakePowerBroker {
    fn new() -> Rc<Self> {
        Rc::new(Default::default())
    }

    fn lock(&self) -> MutexGuard<'_, FakePowerBrokerInner> {
        self.inner.lock().unwrap()
    }

    async fn run(self: &Rc<Self>) -> Result<()> {
        let mut service_fs = ServiceFs::new_local();

        service_fs.dir("svc").add_fidl_service(IncomingRequest::Topology);
        service_fs.take_and_serve_directory_handle().expect("failed to serve outgoing namespace");

        service_fs
            .for_each_concurrent(None, move |request: IncomingRequest| {
                let broker = self.clone();
                async move {
                    match request {
                        IncomingRequest::Topology(stream) => {
                            fasync::Task::local(broker.handle_topology_request(stream)).detach()
                        }
                    }
                }
            })
            .await;
        Ok(())
    }

    async fn handle_topology_request(self: Rc<Self>, mut stream: TopologyRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                TopologyRequest::AddElement { payload, responder, .. } => {
                    let (element_control_client, element_control_stream) =
                        create_request_stream::<ElementControlMarker>().expect("");
                    fasync::Task::local(
                        self.clone().handle_element_control(element_control_stream),
                    )
                    .detach();

                    if let Some(lessor) = payload.lessor_channel {
                        let lessor_stream = lessor.into_stream().unwrap();
                        fasync::Task::local(self.clone().handle_lessor(lessor_stream)).detach();
                    }

                    if let Some(level_control_channels) = payload.level_control_channels {
                        fasync::Task::local(self.clone().handle_current_level_control(
                            level_control_channels.current.into_stream().unwrap(),
                        ))
                        .detach();
                    }

                    if let Some(current_level) = payload.initial_current_level {
                        self.lock().current_level = current_level;
                    }

                    responder.send(Ok(element_control_client)).expect("send should success")
                }
                TopologyRequest::_UnknownMethod { .. } => todo!(),
            }
        }
    }

    async fn handle_element_control(self: Rc<Self>, mut stream: ElementControlRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                ElementControlRequest::OpenStatusChannel { status_channel, .. } => {
                    fasync::Task::local(
                        self.clone().handle_element_status(status_channel.into_stream().unwrap()),
                    )
                    .detach();
                }
                ElementControlRequest::AddDependency { .. } => todo!(),
                ElementControlRequest::RemoveDependency { .. } => todo!(),
                ElementControlRequest::RegisterDependencyToken { .. } => todo!(),
                ElementControlRequest::UnregisterDependencyToken { .. } => todo!(),
                ElementControlRequest::_UnknownMethod { .. } => todo!(),
            }
        }
    }

    async fn handle_element_status(self: Rc<Self>, mut stream: StatusRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                StatusRequest::WatchPowerLevel { responder } => {
                    responder.send(Ok(self.lock().current_level)).expect("send should success")
                }
                StatusRequest::_UnknownMethod { .. } => todo!(),
            }
        }
    }

    async fn handle_lessor(self: Rc<Self>, mut stream: LessorRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                LessorRequest::Lease { responder, .. } => {
                    let (client, stream) = create_request_stream::<LeaseControlMarker>()
                        .expect("should create lease control stream");
                    fasync::Task::local(self.clone().handle_lease_control(stream)).detach();
                    responder.send(Ok(client)).expect("send should success")
                }
                LessorRequest::_UnknownMethod { .. } => todo!(),
            }
        }
    }

    async fn handle_lease_control(self: Rc<Self>, mut stream: LeaseControlRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                LeaseControlRequest::WatchStatus { responder, .. } => {
                    responder.send(LeaseStatus::Satisfied).expect("send should success")
                }
                LeaseControlRequest::_UnknownMethod { .. } => todo!(),
            }
        }
    }

    async fn handle_current_level_control(self: Rc<Self>, mut stream: CurrentLevelRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                CurrentLevelRequest::Update { responder, .. } => {
                    responder.send(Ok(())).expect("send should success")
                }
                CurrentLevelRequest::_UnknownMethod { .. } => todo!(),
            }
        }
    }
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    info!("Start fake power broker component");
    // Set up the SystemActivityGovernor.
    let broker = FakePowerBroker::new();
    broker.run().await
}
