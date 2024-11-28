// Copyright 2024 The Fuchsia Authors. Archivell rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use diagnostics_assertions::assert_data_tree;
use diagnostics_reader::{ArchiveReader, Inspect};
use fidl::endpoints::create_proxy;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use fuchsia_inspect::hierarchy::{DiagnosticsHierarchy, Property};
use futures::channel::mpsc;
use futures::StreamExt;
use unmanaged_element::UnmanagedElement;
use {
    fidl_fuchsia_power_broker as fbroker, fuchsia_async as fasync, power_broker_client as pbclient,
};

/// Manages a test Realm that includes a full Power Broker instance.
struct TestBrokerRealm {
    topology: fbroker::TopologyProxy,
    archive_reader: ArchiveReader,
    _realm: RealmInstance,
}

impl TestBrokerRealm {
    async fn build() -> Result<Self> {
        let builder = RealmBuilder::new().await?;

        // Add Power Broker component to this test realm.
        let power_broker_name = "test-power-broker";
        let power_broker = builder
            .add_child(power_broker_name, "#meta/power-broker.cm", ChildOptions::new())
            .await?;

        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("fuchsia.power.broker.Topology"))
                    .from(&power_broker)
                    .to(Ref::parent()),
            )
            .await?;

        let _realm = builder.build().await?;
        let topology =
            _realm.root.connect_to_protocol_at_exposed_dir::<fbroker::TopologyMarker>()?;
        let mut archive_reader = ArchiveReader::new();
        archive_reader.add_selector(format!(
            "realm_builder\\:{}/{}:root",
            _realm.root.child_name(),
            power_broker_name
        ));
        Ok(Self { topology, archive_reader, _realm })
    }

    /// Should be called at the end of each test to ensure proper teardown between test cases.
    async fn destroy(self) -> Result<()> {
        Ok(self._realm.destroy().await?)
    }
}

/// Gets an element's current power level represented in a Power Broker Inspect Tree.
async fn check_inspect_element_current_level(
    broker_archive_reader: &ArchiveReader,
    element_name: &'static str,
) -> Result<fbroker::PowerLevel> {
    let root = broker_archive_reader
        .snapshot::<Inspect>()
        .await?
        .into_iter()
        .next()
        .and_then(|result| result.payload)
        .expect("snapshot of inspect hierarchy");
    let meta = find_element_meta_node(&root, element_name).expect("finding element node");
    if let Some(Property::Uint(_key, current_level)) = meta.get_property("current_level") {
        Ok(current_level.to_le_bytes()[0])
    } else {
        Err(anyhow!("failed to find current_level property"))
    }
}

/// Searches the provided Inspect hierarchy for a `meta` node matching:
/// `root/broker/topology/fuchsia.inspect.Graph/topology/<ID string>/meta`.
fn find_element_meta_node<'a>(
    root: &'a DiagnosticsHierarchy,
    name: &'a str,
) -> Option<&'a DiagnosticsHierarchy> {
    assert_data_tree!(root, root: contains {
        broker: contains {
            topology: contains { "fuchsia.inspect.Graph": contains { topology: contains {} } } } });
    let topology_node = root
        .get_child_by_path(&["broker", "topology", "fuchsia.inspect.Graph", "topology"])
        .expect("finding topology node");
    for child in topology_node.children.iter() {
        if let Some(meta) = child.get_child("meta") {
            if let Some(Property::String(_key, meta_name)) = meta.get_property("name") {
                if meta_name == name {
                    return Some(meta);
                }
            }
        }
    }
    None
}

#[fuchsia::test]
async fn unmanaged_element_state_propagates_to_status_channel() -> Result<()> {
    let broker_realm = TestBrokerRealm::build().await?;

    // Create the unmanaged power element.
    let element = UnmanagedElement::add(
        &broker_realm.topology,
        "unmanaged-element",
        &pbclient::BINARY_POWER_LEVELS,
        &pbclient::BINARY_POWER_LEVELS[0],
    )
    .await?;

    // Use the unmanaged element's ElementControl client to open a Status channel.
    let (status, status_server) = create_proxy::<fbroker::StatusMarker>();
    element.context.element_control.open_status_channel(status_server)?;

    // Listen for updates on the Status channel.
    let (status_sender, mut status_receiver) = mpsc::unbounded::<fbroker::PowerLevel>();
    let status_task = fasync::Task::spawn(async move {
        loop {
            let power_level = status
                .watch_power_level()
                .await
                .expect("fidl response")
                .expect("power level status");
            status_sender.unbounded_send(power_level).expect("sending status");
        }
    });
    assert_eq!(status_receiver.next().await, Some(pbclient::BINARY_POWER_LEVELS[0]));

    // Change power levels on the unmanaged element and observe updates on the Status channel.
    element.set_level(&pbclient::BINARY_POWER_LEVELS[1]).await?;
    assert_eq!(status_receiver.next().await, Some(pbclient::BINARY_POWER_LEVELS[1]));
    element.set_level(&pbclient::BINARY_POWER_LEVELS[0]).await?;
    assert_eq!(status_receiver.next().await, Some(pbclient::BINARY_POWER_LEVELS[0]));

    drop(status_task);
    broker_realm.destroy().await?;
    Ok(())
}

#[fuchsia::test]
async fn unmanaged_element_state_propagates_to_power_broker_inspect() -> Result<()> {
    let broker_realm = TestBrokerRealm::build().await?;

    // Create the unmanaged power element.
    let element_name = "unmanaged_element";
    let element = UnmanagedElement::add(
        &broker_realm.topology,
        element_name,
        &pbclient::BINARY_POWER_LEVELS,
        &pbclient::BINARY_POWER_LEVELS[0],
    )
    .await?;

    // Check initial power level in Power Broker's Inspect tree.
    assert_eq!(
        check_inspect_element_current_level(&broker_realm.archive_reader, element_name).await?,
        pbclient::BINARY_POWER_LEVELS[0]
    );

    // Change power levels on the unmanaged element and observe updates in Broker Inspect tree.
    element.set_level(&pbclient::BINARY_POWER_LEVELS[1]).await?;
    assert_eq!(
        check_inspect_element_current_level(&broker_realm.archive_reader, element_name).await?,
        pbclient::BINARY_POWER_LEVELS[1]
    );
    element.set_level(&pbclient::BINARY_POWER_LEVELS[0]).await?;
    assert_eq!(
        check_inspect_element_current_level(&broker_realm.archive_reader, element_name).await?,
        pbclient::BINARY_POWER_LEVELS[0]
    );

    broker_realm.destroy().await?;
    Ok(())
}
