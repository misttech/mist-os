// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    diagnostics_assertions::tree_assertion,
    diagnostics_reader::{ArchiveReader, Inspect},
    fidl::endpoints::DiscoverableProtocolMarker,
    fidl_fuchsia_power_topology_test as fpt,
    fuchsia_component_test::{
        Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route, DEFAULT_COLLECTION_NAME,
    },
    tracing::*,
};

macro_rules! block_until_inspect_matches {
    ($moniker:expr, $($tree:tt)+) => {{
        let mut reader = ArchiveReader::new();

        reader
            .select_all_for_moniker($moniker)
            .with_minimum_schema_count(1);

        for i in 0.. {
            let Ok(data) = reader
                .snapshot::<Inspect>()
                .await?
                .into_iter()
                .next()
                .and_then(|result| result.payload)
                .ok_or(anyhow::anyhow!("expected one inspect hierarchy")) else {
                continue;
            };

            let tree_assertion = $crate::tree_assertion!($($tree)+);
            match tree_assertion.run(&data) {
                Ok(_) => break,
                Err(error) => {
                    if i == 10 {
                        tracing::warn!(?error, "Still awaiting inspect match after 10 tries");
                    }
                }
            }
        }
    }};
}

struct TestEnv {
    realm_instance: RealmInstance,
    sag_moniker: String,
    ttd_moniker: String,
}
impl TestEnv {
    /// Connects to a protocol exposed by a component within the RealmInstance.
    pub fn connect_to_protocol<P: DiscoverableProtocolMarker>(&self) -> P::Proxy {
        self.realm_instance.root.connect_to_protocol_at_exposed_dir::<P>().unwrap()
    }
}

async fn create_test_env() -> TestEnv {
    info!("building the test env");

    let builder = RealmBuilder::new().await.unwrap();

    let component_ref = builder
        .add_child("topology-test-daemon", "#meta/topology-test-daemon.cm", ChildOptions::new())
        .await
        .expect("Failed to add child: topology-test-daemon");

    let power_broker_ref = builder
        .add_child("power-broker", "#meta/power-broker.cm", ChildOptions::new())
        .await
        .expect("Failed to add child: power-broker");

    let system_activity_governor_ref = builder
        .add_child(
            "system-activity-governor",
            "#meta/system-activity-governor.cm",
            ChildOptions::new(),
        )
        .await
        .expect("Failed to add child: system-activity-governor");

    // Expose capabilities from power-broker.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.broker.Topology"))
                .from(&power_broker_ref)
                .to(Ref::parent()),
        )
        .await
        .unwrap();

    // Expose capabilities from power-broker to system-activity-governor.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.broker.Topology"))
                .from(&power_broker_ref)
                .to(&system_activity_governor_ref),
        )
        .await
        .unwrap();

    // Expose capabilities from power-broker to topology-test-daemon.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.broker.Topology"))
                .from(&power_broker_ref)
                .to(&component_ref),
        )
        .await
        .unwrap();

    // Expose capabilities from system-activity-governor to topology-test-daemon.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.system.ActivityGovernor"))
                .from(&system_activity_governor_ref)
                .to(&component_ref),
        )
        .await
        .unwrap();

    // Expose capabilities from topology-test-daemon.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name(
                    "fuchsia.power.topology.test.TopologyControl",
                ))
                .capability(Capability::protocol_by_name(
                    "fuchsia.power.topology.test.SystemActivityControl",
                ))
                .from(&component_ref)
                .to(Ref::parent()),
        )
        .await
        .unwrap();

    let realm_instance = builder.build().await.expect("Failed to build RealmInstance");

    let sag_moniker = format!(
        "{}:{}/{}",
        DEFAULT_COLLECTION_NAME,
        realm_instance.root.child_name(),
        "system-activity-governor"
    );
    let ttd_moniker = format!(
        "{}:{}/{}",
        DEFAULT_COLLECTION_NAME,
        realm_instance.root.child_name(),
        "topology-test-daemon"
    );

    TestEnv { realm_instance, sag_moniker, ttd_moniker }
}

#[fuchsia::test]
async fn test_system_activity_control() -> Result<()> {
    let env = create_test_env().await;

    let system_activity_control = env.connect_to_protocol::<fpt::SystemActivityControlMarker>();
    let _ = system_activity_control.start_application_activity().await.unwrap();

    block_until_inspect_matches!(
        &env.sag_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 2u64,
                },
                application_activity: {
                    power_level: 1u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 0u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: -1i64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    let _ = system_activity_control.stop_application_activity().await.unwrap();

    block_until_inspect_matches!(
        &env.sag_moniker,
        root: {
            booting: false,
            power_elements: {
                execution_state: {
                    power_level: 0u64,
                },
                application_activity: {
                    power_level: 0u64,
                },
                full_wake_handling: {
                    power_level: 0u64,
                },
                wake_handling: {
                    power_level: 0u64,
                },
            },
            suspend_stats: {
                success_count: 0u64,
                fail_count: 0u64,
                last_failed_error: 0u64,
                last_time_in_suspend: -1i64,
                last_time_in_suspend_operations: -1i64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}

#[fuchsia::test]
async fn test_invalid_topology() -> Result<()> {
    let env = create_test_env().await;

    let topology_control = env.connect_to_protocol::<fpt::TopologyControlMarker>();
    let element: [fpt::Element; 1] = [fpt::Element {
        element_name: "element1".to_string(),
        initial_current_level: 0,
        valid_levels: vec![0, 1],
        dependencies: vec![fpt::LevelDependency {
            dependency_type: fpt::DependencyType::Active,
            dependent_level: 1,
            requires_element: "element2".to_string(),
            requires_level: 1,
        }],
    }];
    assert_eq!(
        topology_control.create(&element).await.unwrap(),
        Err(fpt::CreateTopologyGraphError::InvalidTopology)
    );

    Ok(())
}

#[fuchsia::test]
async fn test_invalid_element() -> Result<()> {
    let env = create_test_env().await;

    let topology_control = env.connect_to_protocol::<fpt::TopologyControlMarker>();
    let element: [fpt::Element; 1] = [fpt::Element {
        element_name: "element1".to_string(),
        initial_current_level: 0,
        valid_levels: vec![0, 1],
        dependencies: vec![],
    }];
    assert_eq!(topology_control.create(&element).await.unwrap(), Ok(()));

    assert_eq!(
        topology_control.acquire_lease("element2", 1).await.unwrap(),
        Err(fpt::LeaseControlError::InvalidElement)
    );
    assert_eq!(
        topology_control.drop_lease("element2").await.unwrap(),
        Err(fpt::LeaseControlError::InvalidElement)
    );

    Ok(())
}

#[fuchsia::test]
async fn test_topology_control() -> Result<()> {
    let env = create_test_env().await;

    let topology_control = env.connect_to_protocol::<fpt::TopologyControlMarker>();
    // Create a topology of two child elements (C1 & C2) with a shared
    // parent (P) and grandparent (GP)
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
    let element: [fpt::Element; 4] = [
        fpt::Element {
            element_name: "C1".to_string(),
            initial_current_level: 0,
            valid_levels: vec![0, 5],
            dependencies: vec![fpt::LevelDependency {
                dependency_type: fpt::DependencyType::Active,
                dependent_level: 5,
                requires_element: "P".to_string(),
                requires_level: 50,
            }],
        },
        fpt::Element {
            element_name: "C2".to_string(),
            initial_current_level: 0,
            valid_levels: vec![0, 3],
            dependencies: vec![fpt::LevelDependency {
                dependency_type: fpt::DependencyType::Active,
                dependent_level: 3,
                requires_element: "P".to_string(),
                requires_level: 30,
            }],
        },
        fpt::Element {
            element_name: "P".to_string(),
            initial_current_level: 0,
            valid_levels: vec![0, 30, 50],
            dependencies: vec![
                fpt::LevelDependency {
                    dependency_type: fpt::DependencyType::Active,
                    dependent_level: 50,
                    requires_element: "GP".to_string(),
                    requires_level: 200,
                },
                fpt::LevelDependency {
                    dependency_type: fpt::DependencyType::Active,
                    dependent_level: 30,
                    requires_element: "GP".to_string(),
                    requires_level: 90,
                },
            ],
        },
        fpt::Element {
            element_name: "GP".to_string(),
            initial_current_level: 10,
            valid_levels: vec![10, 90, 200],
            dependencies: vec![],
        },
    ];
    let _ = topology_control.create(&element).await.unwrap();

    block_until_inspect_matches!(
        &env.ttd_moniker,
        root: {
            C1: {
                power_level: 0u64,
            },
            C2: {
                power_level: 0u64,
            },
            P: {
                power_level: 0u64,
            },
            GP: {
                power_level: 10u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // Acquire lease for C1 @ 5.
    let _ = topology_control.acquire_lease("C1", 5).await.unwrap();
    block_until_inspect_matches!(
        &env.ttd_moniker,
        root: {
            C1: {
                power_level: 5u64,
            },
            C2: {
                power_level: 0u64,
            },
            P: {
                power_level: 50u64,
            },
            GP: {
                power_level: 200u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // Acquire lease for C2 @ 3.
    let _ = topology_control.acquire_lease("C2", 3).await.unwrap();
    block_until_inspect_matches!(
        &env.ttd_moniker,
        root: {
            C1: {
                power_level: 5u64,
            },
            C2: {
                power_level: 3u64,
            },
            P: {
                power_level: 50u64,
            },
            GP: {
                power_level: 200u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // Drop lease for C1.
    let _ = topology_control.drop_lease("C1").await.unwrap();
    block_until_inspect_matches!(
        &env.ttd_moniker,
        root: {
            C1: {
                power_level: 0u64,
            },
            C2: {
                power_level: 3u64,
            },
            P: {
                power_level: 30u64,
            },
            GP: {
                power_level: 90u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    // Drop lease for C2.
    let _ = topology_control.drop_lease("C2").await.unwrap();
    block_until_inspect_matches!(
        &env.ttd_moniker,
        root: {
            C1: {
                power_level: 0u64,
            },
            C2: {
                power_level: 0u64,
            },
            P: {
                power_level: 0u64,
            },
            GP: {
                power_level: 10u64,
            },
            "fuchsia.inspect.Health": contains {
                status: "OK",
            },
        }
    );

    Ok(())
}
