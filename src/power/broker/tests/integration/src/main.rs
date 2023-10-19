// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl_fuchsia_power_broker::{
    self as fpb, BinaryPowerLevel, Dependency, ElementLevel, LessorMarker, LevelControlMarker,
    LevelControlProxy, PowerLevel, StatusMarker, TopologyMarker,
};
use fuchsia_async;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use futures::channel::mpsc::{unbounded, TryRecvError, UnboundedReceiver};
use futures::StreamExt;
use std::{collections::HashMap, thread, time};

use crate::mock_power_element::MockPowerElement;
mod mock_power_element;

async fn build_power_broker_realm() -> Result<RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;
    let power_broker = builder
        .add_child("power_broker", "power-broker#meta/power-broker.cm", ChildOptions::new())
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LessorMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LevelControlMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<StatusMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<TopologyMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;
    let realm = builder.build().await?;
    Ok(realm)
}

#[fuchsia::test]
async fn test_direct() -> Result<()> {
    let realm = build_power_broker_realm().await?;

    // Create a topology with only two elements and a single dependency:
    // A <- B
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let element_a = topology.add_element("A", Vec::new()).await.expect("add_element failed");
    let element_b = topology.add_element("B", Vec::new()).await.expect("add_element failed");
    topology
        .add_dependency(Dependency {
            level: ElementLevel {
                element: element_b.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: element_a.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");
    tracing::info!("element_a: {:?}", &element_a);
    tracing::info!("element_b: {:?}", &element_b);

    // Create a MockPowerElement that continuously connects to the PowerBroker
    let mut mock_a = MockPowerElement::new(PowerLevel::Binary(BinaryPowerLevel::Off));
    let mock_a_id = element_a.clone();
    tracing::info!("connecting to LevelControl...");
    let client: LevelControlProxy = realm
        .root
        .connect_to_protocol_at_exposed_dir::<LevelControlMarker>()
        .expect("failed to connect to LevelControl service");
    tracing::info!("connected to LevelControl.");
    // Register a channel with the mock to receive PowerLevel updates.
    let (sender_a, mut receiver_a) = unbounded::<PowerLevel>();
    mock_a.set_update_channel(Some(sender_a));
    fuchsia_async::Task::local(async move {
        tracing::info!("connecting mock power element: {:?}", &mock_a_id);
        mock_a.connect(client, &mock_a_id).await.expect("connect failed");
        tracing::info!("disconnected mock power element: {:?}", &mock_a_id);
    })
    .detach();

    let lessor = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let status: fpb::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Wait for the mock element to connect
    tracing::info!("waiting for mock element a ({:?}) to connect...", &element_a);
    let next_level = receiver_a.next().await.expect("receive update failed");
    assert_eq!(next_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let power_level = status.get_power_level(&element_a).await?.expect("get_power_level failed");
    assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    // acquire lease with PB, device should turn on
    let lease_id = lessor
        .lease(&element_b.clone(), &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    let next_level = receiver_a.next().await.expect("receive update failed");
    assert_eq!(next_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let power_level = status.get_power_level(&element_a).await?.expect("get_power_level failed");
    assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::On));

    // drop lease with PB, device should turn off again
    lessor.drop_lease(&lease_id).expect("drop failed");
    let next_level = receiver_a.next().await.expect("receive update failed");
    assert_eq!(next_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let power_level = status.get_power_level(&element_a).await?.expect("get_power_level failed");
    assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    Ok(())
}

#[fuchsia::test]
async fn test_transitive() -> Result<()> {
    let realm = build_power_broker_realm().await?;

    // Create a four element topology with the following dependencies:
    // C depends on B, which in turn depends on A.
    // D has no dependencies or dependents.
    // A <- B <- C   D
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let element_a = topology.add_element("A", Vec::new()).await.expect("add_element failed");
    let element_b = topology.add_element("B", Vec::new()).await.expect("add_element failed");
    let element_c = topology.add_element("C", Vec::new()).await.expect("add_element failed");
    let element_d = topology.add_element("D", Vec::new()).await.expect("add_element failed");
    topology
        .add_dependency(Dependency {
            level: ElementLevel {
                element: element_b.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: element_a.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");
    topology
        .add_dependency(Dependency {
            level: ElementLevel {
                element: element_c.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: element_b.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");

    // Create mock power elements for the managed elements A, B, and D.
    let managed_element_ids = [element_a.clone(), element_b.clone(), element_d.clone()];
    let mut receivers_by_element: HashMap<String, UnboundedReceiver<PowerLevel>> = HashMap::new();
    for element_id in &managed_element_ids {
        let mut mock = MockPowerElement::new(PowerLevel::Binary(BinaryPowerLevel::Off));
        tracing::info!("connecting to LevelControl...");
        let client: LevelControlProxy = realm
            .root
            .connect_to_protocol_at_exposed_dir::<LevelControlMarker>()
            .expect("failed to connect to LevelControl service");
        tracing::info!("connected to LevelControl.");
        let element_id = element_id.clone();
        // Register a channel with the mock to receive PowerLevel updates.
        let (sender, receiver) = unbounded::<PowerLevel>();
        mock.set_update_channel(Some(sender));
        receivers_by_element.insert(element_id.clone(), receiver);
        fuchsia_async::Task::local(async move {
            tracing::info!("connecting mock power element: {:?}", &element_id);
            mock.connect(client, &element_id).await.expect("connect failed");
            tracing::info!("disconnected mock power element: {:?}", &element_id);
        })
        .detach();
    }

    let lessor = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let status: fpb::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Wait for the elements to connect
    for element_id in &managed_element_ids {
        let receiver = receivers_by_element.get_mut(element_id).expect("missing receiver");
        let next_level = receiver.next().await.expect("receive update failed");
        assert_eq!(next_level, PowerLevel::Binary(BinaryPowerLevel::Off));
        let power_level =
            status.get_power_level(&element_id).await?.expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    }

    // Acquire lease for C with PB, A and B should turn on
    // because C has a dependency on B and B has a dependency on A.
    // D should remain off.
    let lease_id = lessor
        .lease(&element_c, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    // TODO(b/302381778): Check power up sequencing here once implemented.
    for element_id in [&element_a, &element_b] {
        let receiver = receivers_by_element.get_mut(element_id).expect("missing receiver");
        let next_level = receiver.next().await.expect("receive update failed");
        assert_eq!(next_level, PowerLevel::Binary(BinaryPowerLevel::On));
        let power_level =
            status.get_power_level(&element_id).await?.expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::On));
    }
    let power_level = status.get_power_level(&element_d).await?;
    assert_eq!(
        power_level,
        Ok(PowerLevel::Binary(BinaryPowerLevel::Off)),
        "Element D should still be OFF"
    );

    // Drop lease for C with PB, both A and B should turn off again.
    // D should remain OFF.
    lessor.drop_lease(&lease_id).expect("drop failed");
    // TODO(b/300144053): Check power down sequencing here once implemented.
    for element_id in [&element_a, &element_b] {
        let receiver = receivers_by_element.get_mut(element_id).expect("missing receiver");
        let next_level = receiver.next().await.expect("receive update failed");
        assert_eq!(next_level, PowerLevel::Binary(BinaryPowerLevel::Off));
        let power_level =
            status.get_power_level(&element_id).await?.expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    }
    let power_level = status.get_power_level(&element_d).await?;
    assert_eq!(
        power_level,
        Ok(PowerLevel::Binary(BinaryPowerLevel::Off)),
        "Element D should still be OFF"
    );

    Ok(())
}

#[fuchsia::test]
async fn test_shared() -> Result<()> {
    let realm = build_power_broker_realm().await?;

    // Create a topology of two child elements with a shared
    // parent and grandparent
    // C1 \
    //     > P -> GP
    // C2 /
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let child1 = topology.add_element("C1", Vec::new()).await.expect("add_element failed");
    let child2 = topology.add_element("C2", Vec::new()).await.expect("add_element failed");
    let parent = topology.add_element("P", Vec::new()).await.expect("add_element failed");
    let grandparent = topology.add_element("GP", Vec::new()).await.expect("add_element failed");
    topology
        .add_dependency(Dependency {
            level: ElementLevel {
                element: child1.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: parent.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");
    topology
        .add_dependency(Dependency {
            level: ElementLevel {
                element: child2.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: parent.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");
    topology
        .add_dependency(Dependency {
            level: ElementLevel {
                element: parent.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: grandparent.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");

    // Create mock power elements for the managed elements P and GP
    let managed_element_ids = [parent.clone(), grandparent.clone()];
    let mut receivers_by_element: HashMap<String, UnboundedReceiver<PowerLevel>> = HashMap::new();
    for element_id in &managed_element_ids {
        let mut mock = MockPowerElement::new(PowerLevel::Binary(BinaryPowerLevel::Off));
        tracing::info!("connecting to LevelControl...");
        let client: LevelControlProxy = realm
            .root
            .connect_to_protocol_at_exposed_dir::<LevelControlMarker>()
            .expect("failed to connect to LevelControl service");
        tracing::info!("connected to LevelControl.");
        // Register a channel with the mock to receive PowerLevel updates.
        let (sender, receiver) = unbounded::<PowerLevel>();
        mock.set_update_channel(Some(sender));
        receivers_by_element.insert(element_id.clone(), receiver);
        let element_id = element_id.clone();
        fuchsia_async::Task::local(async move {
            tracing::info!("connecting mock power element: {:?}", &element_id);
            mock.connect(client, &element_id).await.expect("connect failed");
            tracing::info!("disconnected mock power element: {:?}", &element_id);
        })
        .detach();
    }

    let lessor = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let status: fpb::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Wait for the elements to connect
    for element_id in &managed_element_ids {
        let receiver = receivers_by_element.get_mut(element_id).expect("missing receiver");
        let next_level = receiver.next().await.expect("receive update failed");
        assert_eq!(next_level, PowerLevel::Binary(BinaryPowerLevel::Off));
        let power_level =
            status.get_power_level(&element_id).await?.expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    }

    // Acquire lease for C1, P and GP should turn on
    // because C1 has a dependency on P and P has a dependency on GP.
    let lease_child_1 = lessor
        .lease(&child1, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    // TODO(b/302381778): Check power up sequencing here once implemented.
    for element_id in [&parent, &grandparent] {
        let receiver = receivers_by_element.get_mut(element_id).expect("missing receiver");
        let next_level = receiver.next().await.expect("receive update failed");
        assert_eq!(next_level, PowerLevel::Binary(BinaryPowerLevel::On));
        let power_level =
            status.get_power_level(&element_id).await?.expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::On));
    }

    // Acquire lease for C2, P and GP should remain on.
    let lease_child_2 = lessor
        .lease(&child2, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    // TODO(b/302381778): Use Lease status instead of sleep when available.
    thread::sleep(time::Duration::from_millis(100));
    for element_id in [&parent, &grandparent] {
        let receiver = receivers_by_element.get_mut(element_id).expect("missing receiver");
        let next_level = receiver.try_next();
        assert!(matches!(next_level, Err(TryRecvError { .. })), "level should not have changed");
        let power_level =
            status.get_power_level(&element_id).await?.expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::On));
    }

    // Drop lease for C1, P and GP should remain on because of lease on C2.
    lessor.drop_lease(&lease_child_1).expect("drop failed");
    // TODO(b/302381778): Use Lease status instead of sleep when available.
    thread::sleep(time::Duration::from_millis(100));
    for element_id in [&parent, &grandparent] {
        let receiver = receivers_by_element.get_mut(element_id).expect("missing receiver");
        let next_level = receiver.try_next();
        assert!(matches!(next_level, Err(TryRecvError { .. })), "level should not have changed");
        let power_level =
            status.get_power_level(&element_id).await?.expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::On));
    }

    // Drop lease for C2, P and GP should now turn off.
    lessor.drop_lease(&lease_child_2).expect("drop failed");
    // TODO(b/300144053): Check power down sequencing here once implemented.
    for element_id in [&parent, &grandparent] {
        let receiver = receivers_by_element.get_mut(element_id).expect("missing receiver");
        let next_level = receiver.next().await.expect("receive update failed");
        assert_eq!(next_level, PowerLevel::Binary(BinaryPowerLevel::Off));
        let power_level =
            status.get_power_level(&element_id).await?.expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    }

    Ok(())
}

#[fuchsia::test]
async fn test_topology() -> Result<()> {
    let realm = build_power_broker_realm().await?;

    // Create a four element topology with the following dependencies:
    // C depends on B, which in turn depends on A.
    // D has no dependencies or dependents.
    // A <- B <- C   D
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let water = topology.add_element("Water", Vec::new()).await.expect("add_element failed");
    let earth = topology.add_element("Earth", Vec::new()).await.expect("add_element failed");
    let fire = topology.add_element("Fire", Vec::new()).await.expect("add_element failed");
    let air = topology.add_element("Air", Vec::new()).await.expect("add_element failed");
    topology
        .add_dependency(Dependency {
            level: ElementLevel {
                element: water.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: earth.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");

    let extra_add_dep_res = topology
        .add_dependency(Dependency {
            level: ElementLevel {
                element: water.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: earth.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?;
    assert!(matches!(extra_add_dep_res, Err(fpb::AddDependencyError::AlreadyExists { .. })));

    topology
        .remove_dependency(Dependency {
            level: ElementLevel {
                element: water.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: earth.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("remove_dependency failed");

    let extra_remove_dep_res = topology
        .remove_dependency(Dependency {
            level: ElementLevel {
                element: water.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: earth.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?;
    assert!(matches!(extra_remove_dep_res, Err(fpb::RemoveDependencyError::NotFound { .. })));

    topology.remove_element(&fire).await?.expect("remove_element failed");
    topology.remove_element(&air).await?.expect("remove_element failed");

    let element_not_found_res = topology
        .add_dependency(Dependency {
            level: ElementLevel {
                element: air.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: water.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?;
    assert!(matches!(element_not_found_res, Err(fpb::AddDependencyError::ElementNotFound { .. })));

    let req_element_not_found_res = topology
        .add_dependency(Dependency {
            level: ElementLevel {
                element: earth.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: fire.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?;
    assert!(matches!(
        req_element_not_found_res,
        Err(fpb::AddDependencyError::RequiredElementNotFound { .. })
    ));

    Ok(())
}
