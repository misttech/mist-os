// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use {fidl_fuchsia_power_broker as fbroker, power_broker_client as pbclient};

pub struct UnmanagedElement {
    pub context: pbclient::PowerElementContext,
}

impl UnmanagedElement {
    pub async fn add(
        topology: &fbroker::TopologyProxy,
        name: &str,
        valid_levels: &[fbroker::PowerLevel],
        initial_current_level: &fbroker::PowerLevel,
    ) -> Result<Self> {
        // Build a PowerElementContext with no dependencies (by default) and no dependent elements.
        let context = pbclient::PowerElementContext::builder(topology, name, valid_levels)
            .initial_current_level(*initial_current_level)
            .register_dependency_tokens(false) // Prevent other elements from depending in this one.
            .build()
            .await?;

        Ok(Self { context })
    }

    pub async fn set_level(&self, new_current_level: &fbroker::PowerLevel) -> Result<()> {
        self.context.current_level.update(*new_current_level).await?.map_err(|e| anyhow!("{e:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use fidl::endpoints::create_proxy_and_stream;
    use fuchsia_async as fasync;
    use futures::channel::mpsc;
    use futures::prelude::*;

    struct FakePowerBroker {
        // Forwards ElementSchema received via AddElement requests.
        element_schema_sender: mpsc::UnboundedSender<fbroker::ElementSchema>,
    }

    impl FakePowerBroker {
        async fn run(&self, stream: fbroker::TopologyRequestStream) -> Result<()> {
            stream
                .map(|request| request.context("failed request"))
                .try_for_each(|request| async {
                    match request {
                        fbroker::TopologyRequest::AddElement { payload, responder } => {
                            self.element_schema_sender
                                .unbounded_send(payload)
                                .expect("forwarding element schema");
                            responder.send(Ok(())).context("send failed")
                        }
                        _ => unreachable!(),
                    }
                })
                .await
        }
    }

    struct FakeCurrentLevel {
        // Forwards PowerLevel received via Update requests.
        power_level_sender: mpsc::UnboundedSender<fbroker::PowerLevel>,
    }

    impl FakeCurrentLevel {
        async fn run(&self, stream: fbroker::CurrentLevelRequestStream) -> Result<()> {
            stream
                .map(|request| request.context("failed request"))
                .try_for_each(|request| async {
                    match request {
                        fbroker::CurrentLevelRequest::Update { current_level, responder } => {
                            self.power_level_sender
                                .unbounded_send(current_level)
                                .expect("intercepting power level update");
                            responder.send(Ok(())).context("send failed")
                        }
                        _ => unreachable!(),
                    }
                })
                .await
        }
    }

    #[fasync::run_until_stalled(test)]
    async fn test_add_to_power_topology_sends_correct_schema() -> Result<()> {
        // Create and run the Topology server in the background.
        let (topology, topology_stream) = create_proxy_and_stream::<fbroker::TopologyMarker>()?;
        let (element_schema_sender, mut element_schema_receiver) =
            mpsc::unbounded::<fbroker::ElementSchema>();
        fasync::Task::local(async move {
            let power_broker = FakePowerBroker { element_schema_sender };
            power_broker.run(topology_stream).await.expect("topology server teardown");
        })
        .detach();

        // Create an unmanaged element and add it to the power topology.
        let element_name = "unmanaged-element";
        let valid_levels = &pbclient::BINARY_POWER_LEVELS;
        let initial_level = &pbclient::BINARY_POWER_LEVELS[0]; // Initial state is "off".
        let _ = UnmanagedElement::add(&topology, element_name, valid_levels, initial_level).await?;

        // Check the ElementSchema received by the Topology server.
        let schema = element_schema_receiver.next().await.expect("server AddElement call");
        assert_eq!(schema.element_name, Some(element_name.to_string()));
        assert_eq!(schema.valid_levels, Some(valid_levels.to_vec()));
        assert_eq!(schema.initial_current_level, Some(*initial_level));
        assert_eq!(schema.dependencies, Some(vec![]));
        Ok(())
    }

    #[fasync::run_until_stalled(test)]
    async fn test_set_level_changes_topology_current_level() -> Result<()> {
        // Create and run the Topology server in the background.
        let (topology, topology_stream) = create_proxy_and_stream::<fbroker::TopologyMarker>()?;
        let (element_schema_sender, mut element_schema_receiver) =
            mpsc::unbounded::<fbroker::ElementSchema>();
        fasync::Task::local(async move {
            let power_broker = FakePowerBroker { element_schema_sender };
            power_broker.run(topology_stream).await.expect("topology server teardown");
        })
        .detach();

        // Create an unmanaged element and add it to the power topology.
        let element_name = "unmanaged-element";
        let valid_levels = &pbclient::BINARY_POWER_LEVELS;
        let initial_level = &pbclient::BINARY_POWER_LEVELS[0]; // Initial state is "off".
        let element =
            UnmanagedElement::add(&topology, element_name, valid_levels, initial_level).await?;
        let schema = element_schema_receiver.next().await.expect("server AddElement call");

        // Create a CurrentLevel server to handle Update requests in the background.
        let (power_level_sender, mut power_level_receiver) =
            mpsc::unbounded::<fbroker::PowerLevel>();
        fasync::Task::local(async move {
            let current_level = FakeCurrentLevel { power_level_sender };
            assert!(schema.level_control_channels.is_some());
            let stream = schema.level_control_channels.unwrap().current.into_stream();
            current_level.run(stream).await.expect("current level server teardown");
        })
        .detach();

        element.set_level(&pbclient::BINARY_POWER_LEVELS[1]).await?;
        assert_eq!(power_level_receiver.next().await, Some(pbclient::BINARY_POWER_LEVELS[1]));

        element.set_level(&pbclient::BINARY_POWER_LEVELS[0]).await?;
        assert_eq!(power_level_receiver.next().await, Some(pbclient::BINARY_POWER_LEVELS[0]));

        Ok(())
    }
}
