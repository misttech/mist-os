// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl_fuchsia_power_broker::{
    self as fpb, BinaryPowerLevel, Credential, Dependency, ElementLevel, LessorMarker,
    LevelControlMarker, LevelControlProxy, LevelDependency, Permissions, PowerLevel, StatusMarker,
    TopologyMarker, UserDefinedPowerLevel,
};
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use fuchsia_zircon::{self as zx, HandleBased};

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
    let (parent_token, parent_broker_token) = zx::EventPair::create();
    let parent_cred = Credential {
        broker_token: parent_broker_token,
        permissions: Permissions::READ_POWER_LEVEL
            | Permissions::MODIFY_POWER_LEVEL
            | Permissions::MODIFY_DEPENDENT,
    };
    topology
        .add_element("P", &PowerLevel::Binary(BinaryPowerLevel::Off), vec![], vec![parent_cred])
        .await?
        .expect("add_element failed");
    let (child_token, child_broker_token) = zx::EventPair::create();
    let child_cred = Credential {
        broker_token: child_broker_token,
        permissions: Permissions::MODIFY_POWER_LEVEL
            | Permissions::MODIFY_DEPENDENCY
            | Permissions::ACQUIRE_LEASE,
    };
    topology
        .add_element(
            "C",
            &PowerLevel::Binary(BinaryPowerLevel::Off),
            vec![LevelDependency {
                dependent_level: PowerLevel::Binary(BinaryPowerLevel::On),
                requires: ElementLevel {
                    token: parent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            }],
            vec![child_cred],
        )
        .await?
        .expect("add_element failed");

    let level_control: LevelControlProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<LevelControlMarker>()?;
    let lessor = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let status: fpb::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Initial required level for P should be OFF.
    // Update P's current level to OFF with PowerBroker.
    let parent_req_level = level_control
        .watch_required_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    level_control
        .update_current_power_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::Binary(BinaryPowerLevel::Off),
        )
        .await?
        .expect("update_current_power_level failed");
    let power_level = status
        .get_power_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
        )
        .await?
        .expect("get_power_level failed");
    assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    // Acquire lease for C, P should now have required level ON
    let lease_id = lessor
        .lease(
            child_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::Binary(BinaryPowerLevel::On),
        )
        .await?
        .expect("Lease response not ok");
    let parent_req_level = level_control
        .watch_required_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            Some(&parent_req_level),
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update P's current level to ON. Lease should now be active.
    level_control
        .update_current_power_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::Binary(BinaryPowerLevel::On),
        )
        .await?
        .expect("update_current_power_level failed");
    // TODO(b/302717376): Check Lease status here. Lease should be active.

    // Drop lease, P should now have required level OFF
    lessor.drop_lease(&lease_id).expect("drop failed");
    let parent_req_level = level_control
        .watch_required_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            Some(&parent_req_level),
        )
        .await?
        .expect("watch_required_level failed");
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
    let (element_a_token, element_a_broker_token) = zx::EventPair::create();
    let element_a_cred = Credential {
        broker_token: element_a_broker_token,
        permissions: Permissions::READ_POWER_LEVEL
            | Permissions::MODIFY_POWER_LEVEL
            | Permissions::MODIFY_DEPENDENT,
    };
    topology
        .add_element("A", &PowerLevel::Binary(BinaryPowerLevel::Off), vec![], vec![element_a_cred])
        .await?
        .expect("add_element failed");
    let (element_b_token, element_b_broker_token) = zx::EventPair::create();
    let element_b_cred = Credential {
        broker_token: element_b_broker_token,
        permissions: Permissions::READ_POWER_LEVEL
            | Permissions::MODIFY_POWER_LEVEL
            | Permissions::MODIFY_DEPENDENCY
            | Permissions::MODIFY_DEPENDENT,
    };
    topology
        .add_element(
            "B",
            &PowerLevel::Binary(BinaryPowerLevel::Off),
            vec![LevelDependency {
                dependent_level: PowerLevel::Binary(BinaryPowerLevel::On),
                requires: ElementLevel {
                    token: element_a_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            }],
            vec![element_b_cred],
        )
        .await?
        .expect("add_element failed");
    let (element_c_token, element_c_broker_token) = zx::EventPair::create();
    let element_c_cred = Credential {
        broker_token: element_c_broker_token,
        permissions: Permissions::MODIFY_DEPENDENCY | Permissions::ACQUIRE_LEASE,
    };
    topology
        .add_element(
            "C",
            &PowerLevel::Binary(BinaryPowerLevel::Off),
            vec![LevelDependency {
                dependent_level: PowerLevel::Binary(BinaryPowerLevel::On),
                requires: ElementLevel {
                    token: element_b_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            }],
            vec![element_c_cred],
        )
        .await?
        .expect("add_element failed");
    let (element_d_token, element_d_broker_token) = zx::EventPair::create();
    let element_d_cred = Credential {
        broker_token: element_d_broker_token,
        permissions: Permissions::READ_POWER_LEVEL | Permissions::MODIFY_POWER_LEVEL,
    };
    topology
        .add_element("D", &PowerLevel::Binary(BinaryPowerLevel::Off), vec![], vec![element_d_cred])
        .await?
        .expect("add_element failed");

    let level_control: LevelControlProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<LevelControlMarker>()?;
    let lessor = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let status: fpb::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Initial required level for each element should be OFF.
    // Update managed elements' current level to OFF with PowerBroker.
    for token in [&element_a_token, &element_b_token, &element_d_token] {
        let req_level = level_control
            .watch_required_level(
                token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                None,
            )
            .await?
            .expect("watch_required_level failed");
        assert_eq!(req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
        level_control
            .update_current_power_level(
                token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                &PowerLevel::Binary(BinaryPowerLevel::Off),
            )
            .await?
            .expect("update_current_power_level failed");
        let power_level = status
            .get_power_level(token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"))
            .await?
            .expect("get_power_level failed");
        assert_eq!(power_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    }

    // Acquire lease for C with PB, Initially, A should have required level ON
    // and B should have required level OFF because C has a dependency on B
    // and B has a dependency on A.
    // D should still have required level OFF.
    let lease_id = lessor
        .lease(
            element_c_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::Binary(BinaryPowerLevel::On),
        )
        .await?
        .expect("Lease response not ok");
    let a_req_level = level_control
        .watch_required_level(
            element_a_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let b_req_level = level_control
        .watch_required_level(
            element_b_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let d_req_level = level_control
        .watch_required_level(
            element_d_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update A's current level to ON. Now B's required level should become ON
    // because its dependency on A is unblocked.
    // D should still have required level OFF.
    level_control
        .update_current_power_level(
            element_a_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::Binary(BinaryPowerLevel::On),
        )
        .await?
        .expect("update_current_power_level failed");
    let a_req_level = level_control
        .watch_required_level(
            element_a_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            Some(&PowerLevel::Binary(BinaryPowerLevel::Off)),
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let b_req_level = level_control
        .watch_required_level(
            element_b_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let d_req_level = level_control
        .watch_required_level(
            element_d_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update B's current level to ON.
    // Both A and B should have required_level ON.
    // D should still have required level OFF.
    level_control
        .update_current_power_level(
            element_b_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::Binary(BinaryPowerLevel::On),
        )
        .await?
        .expect("update_current_power_level failed");
    let a_req_level = level_control
        .watch_required_level(
            element_a_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let b_req_level = level_control
        .watch_required_level(
            element_b_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            Some(&PowerLevel::Binary(BinaryPowerLevel::Off)),
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let d_req_level = level_control
        .watch_required_level(
            element_d_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    // TODO(b/302717376): Check Lease status here. Lease should be active.

    // Drop lease for C with PB, B should have required level OFF.
    // A should still have required level ON.
    // D should still have required level OFF.
    lessor.drop_lease(&lease_id).expect("drop failed");
    let a_req_level = level_control
        .watch_required_level(
            element_a_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::On));
    let b_req_level = level_control
        .watch_required_level(
            element_b_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            Some(&PowerLevel::Binary(BinaryPowerLevel::On)),
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let d_req_level = level_control
        .watch_required_level(
            element_d_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));

    // Lower B's current level to OFF
    // Both A and B should have required level OFF.
    // D should still have required level OFF.
    level_control
        .update_current_power_level(
            element_b_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::Binary(BinaryPowerLevel::Off),
        )
        .await?
        .expect("update_current_power_level failed");
    let a_req_level = level_control
        .watch_required_level(
            element_a_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(a_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let b_req_level = level_control
        .watch_required_level(
            element_b_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            Some(&PowerLevel::Binary(BinaryPowerLevel::On)),
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(b_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));
    let d_req_level = level_control
        .watch_required_level(
            element_d_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(d_req_level, PowerLevel::Binary(BinaryPowerLevel::Off));

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
    let (grandparent_token, grandparent_broker_token) = zx::EventPair::create();
    let grandparent_cred = Credential {
        broker_token: grandparent_broker_token,
        permissions: Permissions::READ_POWER_LEVEL
            | Permissions::MODIFY_POWER_LEVEL
            | Permissions::MODIFY_DEPENDENT,
    };
    topology
        .add_element(
            "GP",
            &PowerLevel::UserDefined(UserDefinedPowerLevel { level: 10 }),
            vec![],
            vec![grandparent_cred],
        )
        .await?
        .expect("add_element failed");
    let (parent_token, parent_broker_token) = zx::EventPair::create();
    let parent_cred = Credential {
        broker_token: parent_broker_token,
        permissions: Permissions::READ_POWER_LEVEL
            | Permissions::MODIFY_POWER_LEVEL
            | Permissions::MODIFY_DEPENDENCY
            | Permissions::MODIFY_DEPENDENT,
    };
    topology
        .add_element(
            "P",
            &PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
            vec![
                LevelDependency {
                    dependent_level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 50 }),
                    requires: ElementLevel {
                        token: grandparent_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 }),
                    },
                },
                LevelDependency {
                    dependent_level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 30 }),
                    requires: ElementLevel {
                        token: grandparent_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 90 }),
                    },
                },
            ],
            vec![parent_cred],
        )
        .await?
        .expect("add_element failed");
    let (child1_token, child1_broker_token) = zx::EventPair::create();
    let child1_cred = Credential {
        broker_token: child1_broker_token,
        permissions: Permissions::MODIFY_DEPENDENCY | Permissions::ACQUIRE_LEASE,
    };
    topology
        .add_element(
            "C1",
            &PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
            vec![LevelDependency {
                dependent_level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 5 }),
                requires: ElementLevel {
                    token: parent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 50 }),
                },
            }],
            vec![child1_cred],
        )
        .await?
        .expect("add_element failed");
    let (child2_token, child2_broker_token) = zx::EventPair::create();
    let child2_cred = Credential {
        broker_token: child2_broker_token,
        permissions: Permissions::MODIFY_DEPENDENCY | Permissions::ACQUIRE_LEASE,
    };
    topology
        .add_element(
            "C2",
            &PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
            vec![LevelDependency {
                dependent_level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 3 }),
                requires: ElementLevel {
                    token: parent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    level: PowerLevel::UserDefined(UserDefinedPowerLevel { level: 30 }),
                },
            }],
            vec![child2_cred],
        )
        .await?
        .expect("add_element failed");

    let level_control: LevelControlProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<LevelControlMarker>()?;
    let lessor = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;

    // Initially, Grandparent should have a default required level of 10
    // and Parent should have a default required level of 0.
    let grandparent_req_level = level_control
        .watch_required_level(
            grandparent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(grandparent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 10 }));
    level_control
        .update_current_power_level(
            grandparent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::UserDefined(UserDefinedPowerLevel { level: 10 }),
        )
        .await?
        .expect("update_current_power_level failed");
    let parent_req_level = level_control
        .watch_required_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }));
    level_control
        .update_current_power_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
        )
        .await?
        .expect("update_current_power_level failed");

    // Acquire lease for Child 1. Initially, Grandparent should have
    // required level 200 and Parent should have required level 0
    // because Child 1 has a dependency on Parent and Parent has a
    // dependency on Grandparent. Grandparent has no dependencies so its
    // level should be raised first.
    let lease_child_1 = lessor
        .lease(
            child1_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::UserDefined(UserDefinedPowerLevel { level: 5 }),
        )
        .await?
        .expect("Lease response not ok");
    let grandparent_req_level = level_control
        .watch_required_level(
            grandparent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            Some(&PowerLevel::UserDefined(UserDefinedPowerLevel { level: 10 })),
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(
        grandparent_req_level,
        PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 })
    );
    let parent_req_level = level_control
        .watch_required_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Raise Grandparent's current level to 200. Now Parent claim should
    // be active, because its dependency on Grandparent is unblocked
    // raising its required level to 50.
    level_control
        .update_current_power_level(
            grandparent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 }),
        )
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level = level_control
        .watch_required_level(
            grandparent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(
        grandparent_req_level,
        PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 })
    );
    let parent_req_level = level_control
        .watch_required_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            Some(&PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 })),
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 50 }));
    // TODO(b/302717376): Check Lease status here. Lease should be pending.

    // Update Parent's current level to 50.
    // Parent and Grandparent should have required levels of 50 and 200.
    level_control
        .update_current_power_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::UserDefined(UserDefinedPowerLevel { level: 50 }),
        )
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level = level_control
        .watch_required_level(
            grandparent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(
        grandparent_req_level,
        PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 })
    );
    let parent_req_level = level_control
        .watch_required_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 50 }));
    // TODO(b/302717376): Check Lease status here. Lease should now be active.

    // Acquire lease for Child 2, Though Child 2 has nominal
    // requirements of Parent at 30 and Grandparent at 100, they are
    // superseded by Child 1's requirements of 50 and 200.
    let lease_child_2 = lessor
        .lease(
            child2_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::UserDefined(UserDefinedPowerLevel { level: 3 }),
        )
        .await?
        .expect("Lease response not ok");
    let grandparent_req_level = level_control
        .watch_required_level(
            grandparent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(
        grandparent_req_level,
        PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 })
    );
    let parent_req_level = level_control
        .watch_required_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 50 }));
    // TODO(b/302717376): Check Lease status here. Lease should immediately be active.

    // Drop lease for Child 1. Parent's required level should immediately
    // drop to 30. Grandparent's required level will remain at 200 for now.
    lessor.drop_lease(&lease_child_1).expect("drop failed");
    let grandparent_req_level = level_control
        .watch_required_level(
            grandparent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(
        grandparent_req_level,
        PowerLevel::UserDefined(UserDefinedPowerLevel { level: 200 })
    );
    let parent_req_level = level_control
        .watch_required_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 30 }));
    // TODO(b/302717376): Check Lease status here. Lease 2 should still be active.

    // Lower Parent's current level to 30. Now Grandparent's required level
    // should drop to 90.
    level_control
        .update_current_power_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::UserDefined(UserDefinedPowerLevel { level: 30 }),
        )
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level = level_control
        .watch_required_level(
            grandparent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(grandparent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 90 }));
    let parent_req_level = level_control
        .watch_required_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 30 }));

    // Drop lease for Child 2, Parent should have required level 0.
    // Grandparent should still have required level 90.
    lessor.drop_lease(&lease_child_2).expect("drop failed");
    let grandparent_req_level = level_control
        .watch_required_level(
            grandparent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(grandparent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 90 }));
    let parent_req_level = level_control
        .watch_required_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            Some(&PowerLevel::UserDefined(UserDefinedPowerLevel { level: 30 })),
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }));

    // Lower Parent's current level to 0. Grandparent claim should now be
    // dropped and have its default required level of 10.
    level_control
        .update_current_power_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            &PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }),
        )
        .await?
        .expect("update_current_power_level failed");
    let grandparent_req_level = level_control
        .watch_required_level(
            grandparent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            Some(&PowerLevel::UserDefined(UserDefinedPowerLevel { level: 90 })),
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(grandparent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 10 }));
    let parent_req_level = level_control
        .watch_required_level(
            parent_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
            None,
        )
        .await?
        .expect("watch_required_level failed");
    assert_eq!(parent_req_level, PowerLevel::UserDefined(UserDefinedPowerLevel { level: 0 }));

    Ok(())
}

#[fuchsia::test]
async fn test_topology() -> Result<()> {
    let realm = build_power_broker_realm().await?;

    // Create a four element topology.
    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let (earth_token, earth_broker_token) = zx::EventPair::create();
    let earth_cred =
        Credential { broker_token: earth_broker_token, permissions: Permissions::MODIFY_DEPENDENT };
    topology
        .add_element("Earth", &PowerLevel::Binary(BinaryPowerLevel::Off), vec![], vec![earth_cred])
        .await?
        .expect("add_element failed");
    let (water_token, water_broker_token) = zx::EventPair::create();
    let water_cred = Credential {
        broker_token: water_broker_token,
        permissions: Permissions::MODIFY_DEPENDENCY,
    };
    topology
        .add_element(
            "Water",
            &PowerLevel::Binary(BinaryPowerLevel::Off),
            vec![LevelDependency {
                dependent_level: PowerLevel::Binary(BinaryPowerLevel::On),
                requires: ElementLevel {
                    token: earth_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    level: PowerLevel::Binary(BinaryPowerLevel::On),
                },
            }],
            vec![water_cred],
        )
        .await?
        .expect("add_element failed");
    let (fire_token, fire_broker_token) = zx::EventPair::create();
    let fire_cred =
        Credential { broker_token: fire_broker_token, permissions: Permissions::REMOVE_ELEMENT };
    topology
        .add_element("Fire", &PowerLevel::Binary(BinaryPowerLevel::Off), vec![], vec![fire_cred])
        .await?
        .expect("add_element failed");
    let (air_token, air_broker_token) = zx::EventPair::create();
    let air_cred = Credential { broker_token: air_broker_token, permissions: Permissions::all() };
    let (air_token_no_perms, air_broker_token_no_perms) = zx::EventPair::create();
    let air_cred_no_perms =
        Credential { broker_token: air_broker_token_no_perms, permissions: Permissions::empty() };
    topology
        .add_element(
            "Air",
            &PowerLevel::Binary(BinaryPowerLevel::Off),
            vec![],
            vec![air_cred, air_cred_no_perms],
        )
        .await?
        .expect("add_element failed");

    let extra_add_dep_res = topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: water_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?;
    assert!(matches!(extra_add_dep_res, Err(fpb::AddDependencyError::AlreadyExists { .. })));

    topology
        .remove_dependency(Dependency {
            dependent: ElementLevel {
                token: water_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?
        .expect("remove_dependency failed");

    let extra_remove_dep_res = topology
        .remove_dependency(Dependency {
            dependent: ElementLevel {
                token: water_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?;
    assert!(matches!(extra_remove_dep_res, Err(fpb::RemoveDependencyError::NotFound { .. })));

    topology
        .remove_element(fire_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"))
        .await?
        .expect("remove_element failed");
    let remove_element_not_authorized_res = topology
        .remove_element(
            air_token_no_perms.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
        )
        .await?;
    assert!(matches!(
        remove_element_not_authorized_res,
        Err(fpb::RemoveElementError::NotAuthorized)
    ));
    topology
        .remove_element(air_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"))
        .await?
        .expect("remove_element failed");

    let add_dep_element_not_found_res = topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: air_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: water_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?;
    assert!(matches!(add_dep_element_not_found_res, Err(fpb::AddDependencyError::NotAuthorized)));

    let add_dep_req_element_not_found_res = topology
        .add_dependency(Dependency {
            dependent: ElementLevel {
                token: earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
            requires: ElementLevel {
                token: fire_token.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("dup failed"),
                level: PowerLevel::Binary(BinaryPowerLevel::On),
            },
        })
        .await?;
    assert!(matches!(
        add_dep_req_element_not_found_res,
        Err(fpb::AddDependencyError::NotAuthorized)
    ));

    Ok(())
}

#[fuchsia::test]
async fn test_register_unregister_credentials() -> Result<()> {
    let realm = build_power_broker_realm().await?;

    let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
    let (token_element_owner, token_element_broker) = zx::EventPair::create();
    let broker_credential = fpb::Credential {
        broker_token: token_element_broker,
        permissions: Permissions::READ_POWER_LEVEL
            | Permissions::MODIFY_POWER_LEVEL
            | Permissions::MODIFY_DEPENDENT
            | Permissions::MODIFY_DEPENDENCY
            | Permissions::MODIFY_CREDENTIAL
            | Permissions::REMOVE_ELEMENT,
    };
    topology
        .add_element(
            "element",
            &PowerLevel::Binary(BinaryPowerLevel::Off),
            vec![],
            vec![broker_credential],
        )
        .await?
        .expect("add_element failed");
    let (token_new_owner, token_new_broker) = zx::EventPair::create();
    let credential_to_register = fpb::Credential {
        broker_token: token_new_broker,
        permissions: Permissions::READ_POWER_LEVEL | Permissions::MODIFY_POWER_LEVEL,
    };
    topology
        .register_credentials(
            token_element_owner
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("duplicate_handle failed"),
            vec![credential_to_register],
        )
        .await?
        .expect("register_credentials failed");

    let (_, token_not_authorized_broker) = zx::EventPair::create();
    let credential_not_authorized = fpb::Credential {
        broker_token: token_not_authorized_broker,
        permissions: Permissions::READ_POWER_LEVEL | Permissions::MODIFY_POWER_LEVEL,
    };
    let res_not_authorized = topology
        .register_credentials(
            token_new_owner
                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                .expect("duplicate_handle failed"),
            vec![credential_not_authorized],
        )
        .await?;
    assert!(matches!(res_not_authorized, Err(fpb::RegisterCredentialsError::NotAuthorized)));

    topology
        .unregister_credentials(token_element_owner, vec![token_new_owner])
        .await?
        .expect("unregister_credentials failed");

    Ok(())
}
