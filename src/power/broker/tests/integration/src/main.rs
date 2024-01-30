// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl::endpoints::{create_endpoints, Proxy};
use fidl_fuchsia_power_broker::{
    self as fpb, BinaryPowerLevel, DependencyType, LeaseStatus, LevelDependency, StatusMarker,
    TopologyMarker,
};
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use fuchsia_zircon::{self as zx, HandleBased};
use power_broker_client::BINARY_POWER_LEVELS;

async fn build_power_broker_realm() -> Result<RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;
    let power_broker = builder
        .add_child("power_broker", "power-broker#meta/power-broker.cm", ChildOptions::new())
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
    let parent_token = zx::Event::create();
    let (parent_element_control, _, parent_level_control) = topology
        .add_element(
            "P",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![],
            vec![parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed")],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let parent_status = {
        let (client, server) = create_endpoints::<StatusMarker>();
        parent_element_control.into_proxy()?.open_status_channel(server)?;
        client.into_proxy()?
    };
    let parent_level_control = parent_level_control.into_proxy()?;
    let (_, child_lessor, _) = topology
        .add_element(
            "C",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![LevelDependency {
                dependency_type: DependencyType::Active,
                dependent_level: BinaryPowerLevel::On.into_primitive(),
                requires_token: parent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                requires_level: BinaryPowerLevel::On.into_primitive(),
            }],
            vec![],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let child_lessor = child_lessor.into_proxy()?;

    // Initial required level for P should be OFF.
    // Update P's current level to OFF with PowerBroker.
    let parent_req_level =
        parent_level_control.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(parent_req_level, BinaryPowerLevel::Off.into_primitive());
    parent_level_control
        .update_current_power_level(BinaryPowerLevel::Off.into_primitive())
        .await?
        .expect("update_current_power_level failed");
    let power_level = parent_status.get_power_level().await?.expect("get_power_level failed");
    assert_eq!(power_level, BinaryPowerLevel::Off.into_primitive());

    // Acquire lease for C, P should now have required level ON
    let lease = child_lessor
        .lease(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("Lease response not ok")
        .into_proxy()?;
    let parent_req_level = parent_level_control
        .watch_required_level(parent_req_level)
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, BinaryPowerLevel::On.into_primitive());
    assert_eq!(lease.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Pending);

    // Update P's current level to ON. Lease should now be active.
    parent_level_control
        .update_current_power_level(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("update_current_power_level failed");
    assert_eq!(lease.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Satisfied);

    // Drop lease, P should now have required level OFF
    drop(lease);
    let parent_req_level = parent_level_control
        .watch_required_level(parent_req_level)
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, BinaryPowerLevel::Off.into_primitive());

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
    let element_a_token = zx::Event::create();
    let (element_a_element_control, _, element_a_level_control) = topology
        .add_element(
            "A",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![],
            vec![element_a_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed")],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let element_a_level_control = element_a_level_control.into_proxy()?;
    let element_a_status = {
        let (client, server) = create_endpoints::<StatusMarker>();
        element_a_element_control.into_proxy()?.open_status_channel(server)?;
        client.into_proxy()?
    };
    let element_b_token = zx::Event::create();
    let (element_b_element_control, _, element_b_level_control) = topology
        .add_element(
            "B",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![LevelDependency {
                dependency_type: DependencyType::Active,
                dependent_level: BinaryPowerLevel::On.into_primitive(),
                requires_token: element_a_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                requires_level: BinaryPowerLevel::On.into_primitive(),
            }],
            vec![element_b_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed")],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let element_b_level_control = element_b_level_control.into_proxy()?;
    let element_b_status: fpb::StatusProxy = {
        let (client, server) = create_endpoints::<StatusMarker>();
        element_b_element_control.into_proxy()?.open_status_channel(server)?;
        client.into_proxy()?
    };
    let (_, element_c_lessor, _) = topology
        .add_element(
            "C",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![LevelDependency {
                dependency_type: DependencyType::Active,
                dependent_level: BinaryPowerLevel::On.into_primitive(),
                requires_token: element_b_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                requires_level: BinaryPowerLevel::On.into_primitive(),
            }],
            vec![],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let element_c_lessor = element_c_lessor.into_proxy()?;
    let (element_d_element_control, _, element_d_level_control) = topology
        .add_element(
            "D",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![],
            vec![],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let element_d_level_control = element_d_level_control.into_proxy()?;
    let element_d_status: fpb::StatusProxy = {
        let (client, server) = create_endpoints::<StatusMarker>();
        element_d_element_control.into_proxy()?.open_status_channel(server)?;
        client.into_proxy()?
    };

    // Initial required level for each element should be OFF.
    // Update managed elements' current level to OFF with PowerBroker.
    for (status, level_control) in [
        (&element_a_status, &element_a_level_control),
        (&element_b_status, &element_b_level_control),
        (&element_d_status, &element_d_level_control),
    ] {
        let req_level =
            level_control.get_required_level().await?.expect("get_required_level failed");
        assert_eq!(req_level, BinaryPowerLevel::Off.into_primitive());
        level_control
            .update_current_power_level(BinaryPowerLevel::Off.into_primitive())
            .await?
            .expect("update_current_power_level failed");
        let power_level = status.get_power_level().await?.expect("get_power_level failed");
        assert_eq!(power_level, BinaryPowerLevel::Off.into_primitive());
    }

    // Acquire lease for C with PB, Initially, A should have required level ON
    // and B should have required level OFF because C has a dependency on B
    // and B has a dependency on A.
    // D should still have required level OFF.
    let lease = element_c_lessor
        .lease(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("Lease response not ok")
        .into_proxy()?;
    let a_req_level =
        element_a_level_control.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(a_req_level, BinaryPowerLevel::On.into_primitive());
    let b_req_level =
        element_b_level_control.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(b_req_level, BinaryPowerLevel::Off.into_primitive());
    let d_req_level =
        element_d_level_control.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(d_req_level, BinaryPowerLevel::Off.into_primitive());
    assert_eq!(lease.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Pending);

    // Update A's current level to ON. Now B's required level should become ON
    // because its dependency on A is unblocked.
    // D should still have required level OFF.
    element_a_level_control
        .update_current_power_level(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("update_current_power_level failed");
    let a_req_level = element_a_level_control
        .watch_required_level(BinaryPowerLevel::Off.into_primitive())
        .await?
        .expect("watch_required_level failed");
    assert_eq!(a_req_level, BinaryPowerLevel::On.into_primitive());
    let b_req_level =
        element_b_level_control.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(b_req_level, BinaryPowerLevel::On.into_primitive());
    let d_req_level =
        element_d_level_control.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(d_req_level, BinaryPowerLevel::Off.into_primitive());
    assert_eq!(lease.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Pending);

    // Update B's current level to ON.
    // Both A and B should have required_level ON.
    // D should still have required level OFF.
    element_b_level_control
        .update_current_power_level(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("update_current_power_level failed");
    let a_req_level =
        element_a_level_control.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(a_req_level, BinaryPowerLevel::On.into_primitive());
    let b_req_level = element_b_level_control
        .watch_required_level(BinaryPowerLevel::Off.into_primitive())
        .await?
        .expect("watch_required_level failed");
    assert_eq!(b_req_level, BinaryPowerLevel::On.into_primitive());
    let d_req_level =
        element_d_level_control.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(d_req_level, BinaryPowerLevel::Off.into_primitive());
    assert_eq!(lease.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Satisfied);

    // Drop lease for C with PB, B should have required level OFF.
    // A should still have required level ON.
    // D should still have required level OFF.
    drop(lease);
    let a_req_level =
        element_a_level_control.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(a_req_level, BinaryPowerLevel::On.into_primitive());
    let b_req_level = element_b_level_control
        .watch_required_level(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("watch_required_level failed");
    assert_eq!(b_req_level, BinaryPowerLevel::Off.into_primitive());
    let d_req_level =
        element_d_level_control.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(d_req_level, BinaryPowerLevel::Off.into_primitive());

    // Lower B's current level to OFF
    // Both A and B should have required level OFF.
    // D should still have required level OFF.
    element_b_level_control
        .update_current_power_level(BinaryPowerLevel::Off.into_primitive())
        .await?
        .expect("update_current_power_level failed");
    let a_req_level =
        element_a_level_control.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(a_req_level, BinaryPowerLevel::Off.into_primitive());
    let b_req_level = element_b_level_control
        .watch_required_level(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("watch_required_level failed");
    assert_eq!(b_req_level, BinaryPowerLevel::Off.into_primitive());
    let d_req_level =
        element_d_level_control.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(d_req_level, BinaryPowerLevel::Off.into_primitive());

    Ok(())
}

#[fuchsia::test]
async fn test_shared() -> Result<()> {
    // Create a topology of two child elements with a shared
    // parent and grandparent
    // C1 \
    //     > P -> GP
    // C2 /
    // Child 1 requires Parent at 50 to support its own level of 5.
    // Parent requires Grandparent at 200 to support its own level of 50.
    // C1 -> P -> GP
    //  5 -> 50 -> 200
    // Child 2 requires Parent at 30 to support its own level of 3.
    // Parent requires Grandparent at 90 to support its own level of 30.
    // C2 -> P -> GP
    //  3 -> 30 -> 90
    // Grandparent has a default minimum level of 10.
    // All other elements have a default of 0.
    let realm = build_power_broker_realm().await?;
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let grandparent_token = zx::Event::create();
    let (_, _, grandparent_control) = topology
        .add_element(
            "GP",
            10,
            &[10, 90, 200],
            vec![],
            vec![grandparent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed")],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let grandparent_control = grandparent_control.into_proxy()?;
    let parent_token = zx::Event::create();
    let (_, _, parent_control) = topology
        .add_element(
            "P",
            0,
            &[0, 30, 50],
            vec![
                LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: 50,
                    requires_token: grandparent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: 200,
                },
                LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: 30,
                    requires_token: grandparent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: 90,
                },
            ],
            vec![parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed")],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let parent_control = parent_control.into_proxy()?;
    let (_, child1_lessor, _) = topology
        .add_element(
            "C1",
            0,
            &[0, 5],
            vec![LevelDependency {
                dependency_type: DependencyType::Active,
                dependent_level: 5,
                requires_token: parent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                requires_level: 50,
            }],
            vec![],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let child1_lessor = child1_lessor.into_proxy()?;
    let (_, child2_lessor, _) = topology
        .add_element(
            "C2",
            0,
            &[0, 3],
            vec![LevelDependency {
                dependency_type: DependencyType::Active,
                dependent_level: 3,
                requires_token: parent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                requires_level: 30,
            }],
            vec![],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let child2_lessor = child2_lessor.into_proxy()?;

    // Initially, Grandparent should have a default required level of 10
    // and Parent should have a default required level of 0.
    let grandparent_req_level =
        grandparent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(grandparent_req_level, 10);
    grandparent_control
        .update_current_power_level(10)
        .await?
        .expect("update_current_power_level failed");
    let parent_req_level =
        parent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(parent_req_level, 0);
    parent_control.update_current_power_level(0).await?.expect("update_current_power_level failed");

    // Acquire lease for Child 1. Initially, Grandparent should have
    // required level 200 and Parent should have required level 0
    // because Child 1 has a dependency on Parent and Parent has a
    // dependency on Grandparent. Grandparent has no dependencies so its
    // level should be raised first.
    let lease_child_1 =
        child1_lessor.lease(5).await?.expect("Lease response not ok").into_proxy()?;
    let grandparent_req_level =
        grandparent_control.watch_required_level(10).await?.expect("watch_required_level failed");
    assert_eq!(grandparent_req_level, 200);
    let parent_req_level =
        parent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(parent_req_level, 0);
    assert_eq!(lease_child_1.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Pending);

    // Raise Grandparent's current level to 200. Now Parent claim should
    // be active, because its dependency on Grandparent is unblocked
    // raising its required level to 50.
    grandparent_control
        .update_current_power_level(200)
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level =
        grandparent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(grandparent_req_level, 200);
    let parent_req_level =
        parent_control.watch_required_level(0).await?.expect("watch_required_level failed");
    assert_eq!(parent_req_level, 50);
    assert_eq!(lease_child_1.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Pending);

    // Update Parent's current level to 50.
    // Parent and Grandparent should have required levels of 50 and 200.
    parent_control
        .update_current_power_level(50)
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level =
        grandparent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(grandparent_req_level, 200);
    let parent_req_level =
        parent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(parent_req_level, 50);
    assert_eq!(lease_child_1.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Satisfied);

    // Acquire lease for Child 2, Though Child 2 has nominal
    // requirements of Parent at 30 and Grandparent at 100, they are
    // superseded by Child 1's requirements of 50 and 200.
    let lease_child_2 =
        child2_lessor.lease(3).await?.expect("Lease response not ok").into_proxy()?;
    let grandparent_req_level =
        grandparent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(grandparent_req_level, 200);
    let parent_req_level =
        parent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(parent_req_level, 50);
    assert_eq!(lease_child_1.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Satisfied);
    assert_eq!(lease_child_2.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Satisfied);

    // Drop lease for Child 1. Parent's required level should immediately
    // drop to 30. Grandparent's required level will remain at 200 for now.
    drop(lease_child_1);
    let grandparent_req_level =
        grandparent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(grandparent_req_level, 200);
    let parent_req_level =
        parent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(parent_req_level, 30);
    assert_eq!(lease_child_2.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Satisfied);

    // Lower Parent's current level to 30. Now Grandparent's required level
    // should drop to 90.
    parent_control
        .update_current_power_level(30)
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level =
        grandparent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(grandparent_req_level, 90);
    let parent_req_level =
        parent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(parent_req_level, 30);
    assert_eq!(lease_child_2.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Satisfied);

    // Drop lease for Child 2, Parent should have required level 0.
    // Grandparent should still have required level 90.
    drop(lease_child_2);
    let grandparent_req_level =
        grandparent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(grandparent_req_level, 90);
    let parent_req_level =
        parent_control.watch_required_level(30).await?.expect("watch_required_level failed");
    assert_eq!(parent_req_level, 0);

    // Lower Parent's current level to 0. Grandparent claim should now be
    // dropped and have its default required level of 10.
    parent_control.update_current_power_level(0).await?.expect("update_current_power_level failed");
    let grandparent_req_level =
        grandparent_control.watch_required_level(90).await?.expect("watch_required_level failed");
    assert_eq!(grandparent_req_level, 10);
    let parent_req_level =
        parent_control.get_required_level().await?.expect("get_required_level failed");
    assert_eq!(parent_req_level, 0);

    Ok(())
}

#[fuchsia::test]
async fn test_passive() -> Result<()> {
    // B has an active dependency on A.
    // C has a passive dependency on B (and transitively, a passive dependency on A).
    // D has an active dependency on B (and transitively, an active dependency on A).
    //  A     B     C     D
    // ON <= ON
    //       ON <- ON
    //       ON <======= ON
    let realm = build_power_broker_realm().await?;
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let token_a = zx::Event::create();
    let (_, _, level_control_a) = topology
        .add_element(
            "A",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![],
            vec![token_a.duplicate_handle(zx::Rights::SAME_RIGHTS)?],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let level_control_a = level_control_a.into_proxy()?;
    let token_b_active = zx::Event::create();
    let token_b_passive = zx::Event::create();
    let (_, _, level_control_b) = topology
        .add_element(
            "B",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![LevelDependency {
                dependency_type: DependencyType::Active,
                dependent_level: BinaryPowerLevel::On.into_primitive(),
                requires_token: token_a.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                requires_level: BinaryPowerLevel::On.into_primitive(),
            }],
            vec![token_b_active.duplicate_handle(zx::Rights::SAME_RIGHTS)?],
            vec![token_b_passive.duplicate_handle(zx::Rights::SAME_RIGHTS)?],
        )
        .await?
        .expect("add_element failed");
    let level_control_b = level_control_b.into_proxy()?;
    let (_, element_c_lessor, _) = topology
        .add_element(
            "C",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![LevelDependency {
                dependency_type: DependencyType::Passive,
                dependent_level: BinaryPowerLevel::On.into_primitive(),
                requires_token: token_b_passive.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                requires_level: BinaryPowerLevel::On.into_primitive(),
            }],
            vec![],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let element_c_lessor = element_c_lessor.into_proxy()?;
    let (_, element_d_lessor, _) = topology
        .add_element(
            "D",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![LevelDependency {
                dependency_type: DependencyType::Active,
                dependent_level: BinaryPowerLevel::On.into_primitive(),
                requires_token: token_b_active.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                requires_level: BinaryPowerLevel::On.into_primitive(),
            }],
            vec![],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let element_d_lessor = element_d_lessor.into_proxy()?;

    // Initial required level for A and B should be OFF.
    // Set A and B's current level to OFF.
    let req_level_a =
        level_control_a.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(req_level_a, BinaryPowerLevel::Off.into_primitive());
    let req_level_b =
        level_control_b.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(req_level_b, BinaryPowerLevel::Off.into_primitive());
    level_control_a
        .update_current_power_level(BinaryPowerLevel::Off.into_primitive())
        .await?
        .expect("update_current_power_level failed");
    level_control_b
        .update_current_power_level(BinaryPowerLevel::Off.into_primitive())
        .await?
        .expect("update_current_power_level failed");

    // Lease C, A & B's required levels should remain OFF because of
    // passive claim.
    // Lease C should remain unsatisfied.
    // TODO(b/311419716): When we have lease rejection, reject this lease.
    let lease_c = element_c_lessor
        .lease(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("Lease response not ok")
        .into_proxy()?;
    let req_level_a =
        level_control_a.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(req_level_a, BinaryPowerLevel::Off.into_primitive());
    let req_level_b =
        level_control_b.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(req_level_b, BinaryPowerLevel::Off.into_primitive());
    assert_eq!(lease_c.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Pending);

    // Lease D, A should have required level ON because of D's transitive active claim.
    // Lease C & D should become satisfied.
    let lease_d = element_d_lessor
        .lease(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("Lease response not ok")
        .into_proxy()?;
    let req_level_a = level_control_a
        .watch_required_level(BinaryPowerLevel::Off.into_primitive())
        .await?
        .expect("watch_required_level failed");
    assert_eq!(req_level_a, BinaryPowerLevel::On.into_primitive());
    let req_level_b =
        level_control_b.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(req_level_b, BinaryPowerLevel::Off.into_primitive());
    assert_eq!(lease_c.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Pending);
    assert_eq!(lease_d.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Pending);

    // Update A's current level to ON.
    // B should now have required level ON because of D's active claim and
    // its dependency on A being satisfied.
    level_control_a
        .update_current_power_level(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("update_current_power_level failed");
    let req_level_a =
        level_control_a.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(req_level_a, BinaryPowerLevel::On.into_primitive());
    let req_level_b = level_control_b
        .watch_required_level(BinaryPowerLevel::Off.into_primitive())
        .await?
        .expect("watch_required_level failed");
    assert_eq!(req_level_b, BinaryPowerLevel::On.into_primitive());
    assert_eq!(lease_c.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Pending);
    assert_eq!(lease_d.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Pending);

    // Update B's current level to ON.
    // Lease C & D should become satisfied.
    level_control_b
        .update_current_power_level(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("update_current_power_level failed");
    assert_eq!(lease_c.watch_status(LeaseStatus::Pending).await?, LeaseStatus::Satisfied);
    assert_eq!(lease_d.watch_status(LeaseStatus::Pending).await?, LeaseStatus::Satisfied);

    // Drop Lease on D, B's required level should become OFF.
    drop(lease_d);
    let req_level_a =
        level_control_a.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(req_level_a, BinaryPowerLevel::On.into_primitive());
    let req_level_b = level_control_b
        .watch_required_level(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("watch_required_level failed");
    assert_eq!(req_level_b, BinaryPowerLevel::Off.into_primitive());
    // TODO(b/308659273): When we have lease revocation, verify lease C was revoked.

    // Update B's current level to OFF. A's required level should become OFF.
    level_control_b
        .update_current_power_level(BinaryPowerLevel::Off.into_primitive())
        .await?
        .expect("update_current_power_level failed");
    let req_level_a = level_control_a
        .watch_required_level(BinaryPowerLevel::On.into_primitive())
        .await?
        .expect("watch_required_level failed");
    assert_eq!(req_level_a, BinaryPowerLevel::Off.into_primitive());
    let req_level_b =
        level_control_b.get_required_level().await?.expect("watch_required_level failed");
    assert_eq!(req_level_b, BinaryPowerLevel::Off.into_primitive());

    Ok(())
}

#[fuchsia::test]
async fn test_topology() -> Result<()> {
    let realm = build_power_broker_realm().await?;

    // Create a four element topology.
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let earth_token = zx::Event::create();
    let (earth_element_control, _, _) = topology
        .add_element(
            "Earth",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![],
            vec![earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let earth_element_control = earth_element_control.into_proxy()?;
    let water_token = zx::Event::create();
    let (water_element_control, _, _) = topology
        .add_element(
            "Water",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![LevelDependency {
                dependency_type: DependencyType::Active,
                dependent_level: BinaryPowerLevel::On.into_primitive(),
                requires_token: earth_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed"),
                requires_level: BinaryPowerLevel::On.into_primitive(),
            }],
            vec![water_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let water_element_control = water_element_control.into_proxy()?;
    let fire_token = zx::Event::create();
    let (fire_element_control, _, _) = topology
        .add_element(
            "Fire",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![],
            vec![fire_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let fire_element_control = fire_element_control.into_proxy()?;
    let air_token = zx::Event::create();
    let (air_element_control, _, _) = topology
        .add_element(
            "Air",
            BinaryPowerLevel::Off.into_primitive(),
            &BINARY_POWER_LEVELS,
            vec![],
            vec![air_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?],
            vec![],
        )
        .await?
        .expect("add_element failed");
    let air_element_control = air_element_control.into_proxy()?;

    let extra_add_dep_res = water_element_control
        .add_dependency(
            DependencyType::Active,
            BinaryPowerLevel::On.into_primitive(),
            earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
            BinaryPowerLevel::On.into_primitive(),
        )
        .await?;
    assert!(matches!(extra_add_dep_res, Err(fpb::ModifyDependencyError::AlreadyExists { .. })));

    water_element_control
        .remove_dependency(
            DependencyType::Active,
            BinaryPowerLevel::On.into_primitive(),
            earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
            BinaryPowerLevel::On.into_primitive(),
        )
        .await?
        .expect("remove_dependency failed");

    let extra_remove_dep_res = water_element_control
        .remove_dependency(
            DependencyType::Active,
            BinaryPowerLevel::On.into_primitive(),
            earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
            BinaryPowerLevel::On.into_primitive(),
        )
        .await?;
    assert!(matches!(extra_remove_dep_res, Err(fpb::ModifyDependencyError::NotFound { .. })));

    fire_element_control.remove_element().await.expect("remove_element failed");
    fire_element_control.on_closed().await?;
    air_element_control.remove_element().await.expect("remove_element failed");
    air_element_control.on_closed().await?;

    let add_dep_req_invalid = earth_element_control
        .add_dependency(
            DependencyType::Active,
            BinaryPowerLevel::On.into_primitive(),
            fire_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?,
            BinaryPowerLevel::On.into_primitive(),
        )
        .await?;
    assert!(matches!(add_dep_req_invalid, Err(fpb::ModifyDependencyError::NotAuthorized),));

    Ok(())
}
