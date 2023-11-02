// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl_fuchsia_power_broker::{
    self as fpb, BinaryPowerLevel, Dependency, ElementLevel, LessorMarker, LevelControlMarker,
    LevelControlProxy, PowerLevel, StatusMarker, TopologyMarker,
};
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};

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
    // P <- C
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let child = topology.add_element("C", Vec::new()).await.expect("add_element failed");
    let parent = topology.add_element("P", Vec::new()).await.expect("add_element failed");
    topology
        .add_dependency(Dependency {
            level: ElementLevel {
                element: child.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                element: parent.clone(),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("add_dependency failed");

    let level_control: LevelControlProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<LevelControlMarker>()?;
    let lessor = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let status: fpb::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Initial required level for P should be OFF.
    // Update P's current level to OFF with PowerBroker.
    let parent_req_level = level_control.watch_required_level(&parent, None).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    level_control
        .update_current_power_level(&parent, &PowerLevel::Binary(BinaryPowerLevel::Off))
        .await?
        .expect("update_current_power_level failed");
    let power_level = status.get_power_level(&parent).await?.expect("get_power_level failed");
    assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    // Acquire lease for C, P should now have required level ON
    let lease_id = lessor
        .lease(&child, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    let parent_req_level =
        level_control.watch_required_level(&parent, Some(&parent_req_level)).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update P's current level to ON. Lease should now be active.
    level_control
        .update_current_power_level(&parent, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await?
        .expect("update_current_power_level failed");
    // TODO(b/302717376): Check Lease status here. Lease should be active.

    // Drop lease, P should now have required level OFF
    lessor.drop_lease(&lease_id).expect("drop failed");
    let parent_req_level =
        level_control.watch_required_level(&parent, Some(&parent_req_level)).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));

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

    let level_control: LevelControlProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<LevelControlMarker>()?;
    let lessor = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let status: fpb::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Initial required level for each element should be OFF.
    // Update managed elements' current level to OFF with PowerBroker.
    for element_id in [&element_a, &element_b, &element_d] {
        let req_level = level_control.watch_required_level(element_id, None).await?;
        assert_eq!(req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
        level_control
            .update_current_power_level(&element_id, &PowerLevel::Binary(BinaryPowerLevel::Off))
            .await?
            .expect("update_current_power_level failed");
        let power_level =
            status.get_power_level(&element_id).await?.expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    }

    // Acquire lease for C with PB, Initially, A should have required level ON
    // and B should have required level OFF because C has a dependency on B
    // and B has a dependency on A.
    // D should still have required level OFF.
    let lease_id = lessor
        .lease(&element_c, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    let a_req_level = level_control.watch_required_level(&element_a, None).await?;
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let b_req_level = level_control.watch_required_level(&element_b, None).await?;
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let d_req_level = level_control.watch_required_level(&element_d, None).await?;
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update A's current level to ON. Now B's required level should become ON
    // because its dependency on A is unblocked.
    // D should still have required level OFF.
    level_control
        .update_current_power_level(&element_a, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await?
        .expect("update_current_power_level failed");
    let a_req_level = level_control
        .watch_required_level(&element_a, Some(&PowerLevel::Binary(BinaryPowerLevel::Off)))
        .await?;
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let b_req_level = level_control.watch_required_level(&element_b, None).await?;
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let d_req_level = level_control.watch_required_level(&element_d, None).await?;
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update B's current level to ON.
    // Both A and B should have required_level ON.
    // D should still have required level OFF.
    level_control
        .update_current_power_level(&element_b, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await?
        .expect("update_current_power_level failed");
    let a_req_level = level_control.watch_required_level(&element_a, None).await?;
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let b_req_level = level_control
        .watch_required_level(&element_b, Some(&PowerLevel::Binary(BinaryPowerLevel::Off)))
        .await?;
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let d_req_level = level_control.watch_required_level(&element_d, None).await?;
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    // TODO(b/302717376): Check Lease status here. Lease should be active.

    // Drop lease for C with PB, B should have required level OFF.
    // A should still have required level ON.
    // D should still have required level OFF.
    lessor.drop_lease(&lease_id).expect("drop failed");
    let a_req_level = level_control.watch_required_level(&element_a, None).await?;
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let b_req_level = level_control
        .watch_required_level(&element_b, Some(&PowerLevel::Binary(BinaryPowerLevel::On)))
        .await?;
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let d_req_level = level_control.watch_required_level(&element_d, None).await?;
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    // Lower B's current level to OFF
    // Both A and B should have required level OFF.
    // D should still have required level OFF.
    level_control
        .update_current_power_level(&element_b, &PowerLevel::Binary(BinaryPowerLevel::Off))
        .await?
        .expect("update_current_power_level failed");
    let a_req_level = level_control.watch_required_level(&element_a, None).await?;
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let b_req_level = level_control
        .watch_required_level(&element_b, Some(&PowerLevel::Binary(BinaryPowerLevel::On)))
        .await?;
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let d_req_level = level_control.watch_required_level(&element_d, None).await?;
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));

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

    let level_control: LevelControlProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<LevelControlMarker>()?;
    let lessor = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let status: fpb::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Initial required level for each element should be OFF.
    // Update all elements' current level to OFF with PowerBroker.
    for element_id in [&parent, &grandparent] {
        let req_level = level_control.watch_required_level(element_id, None).await?;
        assert_eq!(req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
        level_control
            .update_current_power_level(&element_id, &PowerLevel::Binary(BinaryPowerLevel::Off))
            .await?
            .expect("update_current_power_level failed");
        let power_level =
            status.get_power_level(&element_id).await?.expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    }

    // Acquire lease for C1. Initially, GP should have required level ON
    // and P should have required level OFF because C1 has a dependency on P
    // and P has a dependency on GP. GP has no dependencies so it should be
    // turned on first.
    let lease_child_1 = lessor
        .lease(&child1, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    let grandparent_req_level = level_control
        .watch_required_level(&grandparent, Some(&PowerLevel::Binary(BinaryPowerLevel::Off)))
        .await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let parent_req_level = level_control.watch_required_level(&parent, None).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update GP's current level to ON. Now P's required level should become ON
    // because its dependency on GP is unblocked.
    level_control
        .update_current_power_level(&grandparent, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level = level_control.watch_required_level(&grandparent, None).await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let parent_req_level = level_control
        .watch_required_level(&parent, Some(&PowerLevel::Binary(BinaryPowerLevel::Off)))
        .await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update P's current level to ON.
    // Both P and GP should have required_level ON.
    level_control
        .update_current_power_level(&parent, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level = level_control.watch_required_level(&grandparent, None).await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let parent_req_level = level_control.watch_required_level(&parent, None).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    // TODO(b/302717376): Check Lease status here. Lease should now be active.

    // Acquire lease for C2, P and GP should still have required_level ON.
    let lease_child_2 = lessor
        .lease(&child2, &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    let grandparent_req_level = level_control.watch_required_level(&grandparent, None).await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let parent_req_level = level_control.watch_required_level(&parent, None).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    // TODO(b/302717376): Check Lease status here. Lease should immediately be active.

    // Drop lease for C1, P and GP should still have required_level ON
    // because of lease on C2.
    lessor.drop_lease(&lease_child_1).expect("drop failed");
    let grandparent_req_level = level_control.watch_required_level(&grandparent, None).await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let parent_req_level = level_control.watch_required_level(&parent, None).await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    // TODO(b/302717376): Check Lease status here. Lease 2 should still be active.

    // Drop lease for C2, P should have required level OFF.
    // GP should still have required level ON.
    lessor.drop_lease(&lease_child_2).expect("drop failed");
    let grandparent_req_level = level_control.watch_required_level(&grandparent, None).await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let parent_req_level = level_control
        .watch_required_level(&parent, Some(&PowerLevel::Binary(BinaryPowerLevel::On)))
        .await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    // Lower P's current level to OFF
    // Both P and GP should have required level OFF.
    level_control
        .update_current_power_level(&parent, &PowerLevel::Binary(BinaryPowerLevel::Off))
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level = level_control.watch_required_level(&grandparent, None).await?;
    assert_eq!(grandparent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let parent_req_level = level_control
        .watch_required_level(&parent, Some(&PowerLevel::Binary(BinaryPowerLevel::On)))
        .await?;
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));

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
