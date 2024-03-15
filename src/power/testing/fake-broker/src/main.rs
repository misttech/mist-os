// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::rc::Rc;

use anyhow::{Error, Result};
use fidl::endpoints::create_request_stream;
use fidl_fuchsia_power_broker::{
    ElementControlMarker, LeaseControlMarker, LessorMarker, LessorRequest, LessorRequestStream,
    LevelControlMarker, TopologyRequest, TopologyRequestStream,
};
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt, TryStreamExt};
use tracing::info;

enum IncomingRequest {
    Topology(TopologyRequestStream),
}

struct FakePowerBroker;

impl FakePowerBroker {
    fn new() -> Rc<Self> {
        Rc::new(Self {})
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
                TopologyRequest::AddElement { responder, .. } => {
                    let (element_control_client, _element_control_stream) =
                        create_request_stream::<ElementControlMarker>().expect("");

                    let (lessor_client, lessor_stream) =
                        create_request_stream::<LessorMarker>().expect("");
                    fasync::Task::local(self.clone().handle_lessor(lessor_stream)).detach();

                    let (level_control_client, _level_control_stream) =
                        create_request_stream::<LevelControlMarker>().expect("");

                    responder
                        .send(Ok((element_control_client, lessor_client, level_control_client)))
                        .expect("send should success")
                }
                TopologyRequest::_UnknownMethod { .. } => todo!(),
            }
        }
    }

    async fn handle_lessor(self: Rc<Self>, mut stream: LessorRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                LessorRequest::Lease { responder, .. } => {
                    let (client, _stream) = create_request_stream::<LeaseControlMarker>()
                        .expect("should create lease control stream");
                    responder.send(Ok(client)).expect("send should success")
                }
                LessorRequest::_UnknownMethod { .. } => todo!(),
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
