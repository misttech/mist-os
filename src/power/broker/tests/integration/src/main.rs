// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl::endpoints::{create_endpoints, create_proxy, Proxy};
use fidl_fuchsia_power_broker::{
    self as fpb, BinaryPowerLevel, DependencyType, ElementSchema, LeaseStatus, LevelDependency,
    StatusMarker, TopologyMarker,
};
use fuchsia_async as fasync;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use fuchsia_zircon::{self as zx, HandleBased};
use futures::task::Poll;
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

#[cfg(test)]
mod tests {
    use {
        super::*,
        fpb::{CurrentLevelMarker, RequiredLevelMarker},
    };

    #[fuchsia::test]
    async fn test_direct() -> Result<()> {
        let realm = build_power_broker_realm().await?;

        // Create a topology with only two elements and a single dependency:
        // P <- C
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
        let parent_token = zx::Event::create();
        let (parent_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (parent_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let parent_element_control = topology
            .add_element(ElementSchema {
                element_name: Some("P".into()),
                initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                active_dependency_tokens_to_register: Some(vec![parent_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .expect("dup failed")]),
                level_control_channels: Some(fpb::LevelControlChannels {
                    current: current_server,
                    required: required_server,
                }),
                ..Default::default()
            })
            .await?
            .expect("add_element failed");
        let parent_element_control = parent_element_control.into_proxy()?;
        let parent_status = {
            let (client, server) = create_proxy::<StatusMarker>()?;
            parent_element_control.open_status_channel(server)?;
            client
        };
        let (child_lessor, lessor_server) = create_proxy::<fpb::LessorMarker>()?;
        let _child_element_control = topology
            .add_element(ElementSchema {
                element_name: Some("C".into()),
                initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                dependencies: Some(vec![LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: parent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }]),
                lessor_channel: Some(lessor_server),
                ..Default::default()
            })
            .await?
            .expect("add_element failed");

        // Initial required level for P should be OFF.
        // Update P's current level to OFF with PowerBroker.
        let parent_req_level = parent_required.watch().await?.expect("watch_required_level failed");
        assert_eq!(parent_req_level, BinaryPowerLevel::Off.into_primitive());
        parent_current
            .update(BinaryPowerLevel::Off.into_primitive())
            .await?
            .expect("update_current_power_level failed");
        let power_level =
            parent_status.watch_power_level().await?.expect("watch_power_level failed");
        assert_eq!(power_level, BinaryPowerLevel::Off.into_primitive());

        // Acquire lease for C, P should now have required level ON
        let lease = child_lessor
            .lease(BinaryPowerLevel::On.into_primitive())
            .await?
            .expect("Lease response not ok")
            .into_proxy()?;
        let parent_req_level = parent_required.watch().await?.expect("watch_required_level failed");
        assert_eq!(parent_req_level, BinaryPowerLevel::On.into_primitive());
        assert_eq!(lease.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Pending);

        // Update P's current level to ON. Lease should now be active.
        parent_current
            .update(BinaryPowerLevel::On.into_primitive())
            .await?
            .expect("update_current_power_level failed");
        let power_level =
            parent_status.watch_power_level().await?.expect("watch_power_level failed");
        assert_eq!(power_level, BinaryPowerLevel::On.into_primitive());
        assert_eq!(lease.watch_status(LeaseStatus::Unknown).await?, LeaseStatus::Satisfied);

        // Drop lease, P should now have required level OFF
        drop(lease);
        let parent_req_level = parent_required.watch().await?.expect("watch_required_level failed");
        assert_eq!(parent_req_level, BinaryPowerLevel::Off.into_primitive());

        // Update P's required level to OFF
        parent_current
            .update(BinaryPowerLevel::Off.into_primitive())
            .await?
            .expect("update_current_power_level failed");
        let power_level =
            parent_status.watch_power_level().await?.expect("watch_power_level failed");
        assert_eq!(power_level, BinaryPowerLevel::Off.into_primitive());

        // Remove P's element. Status channel should be closed.
        drop(parent_element_control);
        let status_after_remove = parent_status.watch_power_level().await;
        assert!(matches!(status_after_remove, Err(fidl::Error::ClientChannelClosed { .. })));

        Ok(())
    }

    #[test]
    fn test_transitive() -> Result<()> {
        let mut executor = fasync::TestExecutor::new();
        let realm = executor.run_singlethreaded(async { build_power_broker_realm().await })?;

        // Create a four element topology with the following dependencies:
        // C depends on B, which in turn depends on A.
        // D has no dependencies or dependents.
        // A <- B <- C   D
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
        let element_a_token = zx::Event::create();
        let (element_a_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (element_a_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let element_a_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("A".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    active_dependency_tokens_to_register: Some(vec![element_a_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let element_a_element_control = element_a_element_control.into_proxy()?;
        let element_a_status = {
            let (client, server) = create_endpoints::<StatusMarker>();
            element_a_element_control.open_status_channel(server)?;
            client.into_proxy()?
        };
        let element_b_token = zx::Event::create();
        let (element_b_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (element_b_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let element_b_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("B".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: element_a_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    }]),
                    active_dependency_tokens_to_register: Some(vec![element_b_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let element_b_element_control = element_b_element_control.into_proxy()?;
        let element_b_status: fpb::StatusProxy = {
            let (client, server) = create_endpoints::<StatusMarker>();
            element_b_element_control.open_status_channel(server)?;
            client.into_proxy()?
        };
        let (element_c_lessor, lessor_server) = create_proxy::<fpb::LessorMarker>()?;
        let _element_c_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("C".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: element_b_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    }]),
                    lessor_channel: Some(lessor_server),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let (element_d_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (element_d_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let element_d_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("D".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let element_d_element_control = element_d_element_control.into_proxy()?;
        let element_d_status: fpb::StatusProxy = {
            let (client, server) = create_endpoints::<StatusMarker>();
            element_d_element_control.open_status_channel(server)?;
            client.into_proxy()?
        };

        // Initial required level for each element should be OFF.
        // Update managed elements' current level to OFF with PowerBroker.
        for (status, current, required) in [
            (&element_a_status, &element_a_current, &element_a_required),
            (&element_b_status, &element_b_current, &element_b_required),
            (&element_d_status, &element_d_current, &element_d_required),
        ] {
            executor.run_singlethreaded(async {
                let req_level_fut = required.watch();
                assert_eq!(
                    req_level_fut.await.unwrap(),
                    Ok(BinaryPowerLevel::Off.into_primitive())
                );
                current
                    .update(BinaryPowerLevel::Off.into_primitive())
                    .await
                    .unwrap()
                    .expect("update_current_power_level failed");
                let power_level =
                    status.watch_power_level().await.unwrap().expect("watch_power_level failed");
                assert_eq!(power_level, BinaryPowerLevel::Off.into_primitive());
            });
        }
        let element_a_required_fut = element_a_required.watch();
        let mut element_b_required_fut = element_b_required.watch();
        let mut element_d_required_fut = element_d_required.watch();

        // Acquire lease for C with PB, Initially, A should have required level ON
        // and B should have required level OFF because C has a dependency on B
        // and B has a dependency on A.
        // D should still have required level OFF.
        let lease = executor.run_singlethreaded(async {
            element_c_lessor
                .lease(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("Lease response not ok")
                .into_proxy()
        })?;
        executor.run_singlethreaded(async {
            assert_eq!(
                element_a_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::On.into_primitive())
            );
        });
        let mut element_a_required_fut = element_a_required.watch();
        assert_eq!(
            executor.run_until_stalled(&mut element_b_required_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut element_d_required_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(
                lease.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Update A's current level to ON. Now B's required level should become ON
        // because its dependency on A is unblocked.
        // D should still have required level OFF.
        executor.run_singlethreaded(async {
            element_a_current
                .update(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        assert_eq!(
            executor.run_until_stalled(&mut element_a_required_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(
                element_b_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::On.into_primitive())
            );
        });
        let mut element_b_required_fut = element_b_required.watch();
        assert_eq!(
            executor.run_until_stalled(&mut element_d_required_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(
                lease.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Update B's current level to ON.
        // The lease for C should now be satisfied.
        // Both A and B should have required_level ON.
        // D should still have required level OFF.
        executor.run_singlethreaded(async {
            element_b_current
                .update(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        assert_eq!(
            executor.run_until_stalled(&mut element_a_required_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut element_b_required_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut element_d_required_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(
                lease.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Drop lease for C with PB, B should have required level OFF.
        // A should still have required level ON.
        // D should still have required level OFF.
        drop(lease);
        assert_eq!(
            executor.run_until_stalled(&mut element_a_required_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(
                element_b_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::Off.into_primitive())
            );
        });
        let mut element_b_required_fut = element_b_required.watch();
        assert_eq!(
            executor.run_until_stalled(&mut element_d_required_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );

        // Lower B's current level to OFF
        // A should now have required level OFF.
        // B should still have required level OFF.
        // D should still have required level OFF.
        executor.run_singlethreaded(async {
            element_b_current
                .update(BinaryPowerLevel::Off.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        executor.run_singlethreaded(async {
            assert_eq!(
                element_a_required_fut.await.unwrap(),
                Ok(BinaryPowerLevel::Off.into_primitive())
            );
        });
        assert_eq!(
            executor.run_until_stalled(&mut element_b_required_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut element_d_required_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );

        Ok(())
    }

    #[test]
    fn test_shared() -> Result<()> {
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
        let mut executor = fasync::TestExecutor::new();
        let realm = executor.run_singlethreaded(async { build_power_broker_realm().await })?;
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
        let grandparent_token = zx::Event::create();
        let (grandparent_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (grandparent_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let _grandparent_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("GP".into()),
                    initial_current_level: Some(10),
                    valid_levels: Some(vec![10, 90, 200]),
                    active_dependency_tokens_to_register: Some(vec![grandparent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let parent_token = zx::Event::create();
        let (parent_current, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (parent_required, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let _parent_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("P".into()),
                    initial_current_level: Some(0),
                    valid_levels: Some(vec![0, 30, 50]),
                    dependencies: Some(vec![
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
                    ]),
                    active_dependency_tokens_to_register: Some(vec![parent_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed")]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let (child1_lessor, lessor_server) = create_proxy::<fpb::LessorMarker>()?;
        let _child1_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("C1".into()),
                    initial_current_level: Some(0),
                    valid_levels: Some(vec![0, 5]),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 5,
                        requires_token: parent_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: 50,
                    }]),
                    lessor_channel: Some(lessor_server),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let (child2_lessor, lessor_server) = create_proxy::<fpb::LessorMarker>()?;
        let _child2_element_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("C2".into()),
                    initial_current_level: Some(0),
                    valid_levels: Some(vec![0, 3]),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: 3,
                        requires_token: parent_token
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .expect("dup failed"),
                        requires_level: 30,
                    }]),
                    lessor_channel: Some(lessor_server),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });

        // Initially, Grandparent should have a default required level of 10
        // and Parent should have a default required level of 0.
        executor.run_singlethreaded(async {
            let grandparent_req_level_fut = grandparent_required.watch();
            let parent_req_level_fut = parent_required.watch();
            assert_eq!(grandparent_req_level_fut.await.unwrap(), Ok(10));
            grandparent_current
                .update(10)
                .await
                .unwrap()
                .expect("update_current_power_level failed");
            assert_eq!(parent_req_level_fut.await.unwrap(), Ok(0));
            parent_current.update(0).await.unwrap().expect("update_current_power_level failed");
        });
        let grandparent_req_level_fut = grandparent_required.watch();
        let mut parent_req_level_fut = parent_required.watch();

        // Acquire lease for Child 1. Initially, Grandparent should have
        // required level 200 and Parent should have required level 0
        // because Child 1 has a dependency on Parent and Parent has a
        // dependency on Grandparent. Grandparent has no dependencies so its
        // level should be raised first.
        let lease_child_1 = executor.run_singlethreaded(async {
            child1_lessor.lease(5).await.unwrap().expect("Lease response not ok").into_proxy()
        })?;
        executor.run_singlethreaded(async {
            assert_eq!(grandparent_req_level_fut.await.unwrap(), Ok(200));
        });
        let mut grandparent_req_level_fut = grandparent_required.watch();
        assert_eq!(
            executor.run_until_stalled(&mut parent_req_level_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_child_1.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Raise Grandparent's current level to 200. Now Parent claim should
        // be active, because its dependency on Grandparent is unblocked
        // raising its required level to 50.
        executor.run_singlethreaded(async {
            grandparent_current
                .update(200)
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        assert_eq!(
            executor.run_until_stalled(&mut grandparent_req_level_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(parent_req_level_fut.await.unwrap(), Ok(50));
            assert_eq!(
                lease_child_1.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });
        let mut parent_req_level_fut = parent_required.watch();

        // Update Parent's current level to 50.
        // Parent and Grandparent should have required levels of 50 and 200.
        executor.run_singlethreaded(async {
            parent_current.update(50).await.unwrap().expect("update_current_power_level failed");
        });
        assert_eq!(
            executor.run_until_stalled(&mut grandparent_req_level_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut parent_req_level_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_child_1.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Acquire lease for Child 2, Though Child 2 has nominal
        // requirements of Parent at 30 and Grandparent at 100, they are
        // superseded by Child 1's requirements of 50 and 200.
        let lease_child_2 = executor.run_singlethreaded(async {
            child2_lessor
                .lease(3)
                .await
                .unwrap()
                .expect("Lease response not ok")
                .into_proxy()
                .unwrap()
        });
        assert_eq!(
            executor.run_until_stalled(&mut grandparent_req_level_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut parent_req_level_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_child_1.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
            assert_eq!(
                lease_child_2.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Drop lease for Child 1. Parent's required level should immediately
        // drop to 30. Grandparent's required level will remain at 200 for now.
        drop(lease_child_1);
        assert_eq!(
            executor.run_until_stalled(&mut grandparent_req_level_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(parent_req_level_fut.await.unwrap(), Ok(30));
            assert_eq!(
                lease_child_2.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });
        let mut parent_req_level_fut = parent_required.watch();

        // Lower Parent's current level to 30. Now Grandparent's required level
        // should drop to 90.
        executor.run_singlethreaded(async {
            parent_current.update(30).await.unwrap().expect("update_current_power_level failed");
            assert_eq!(grandparent_req_level_fut.await.unwrap(), Ok(90));
        });
        assert_eq!(
            executor.run_until_stalled(&mut parent_req_level_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_child_2.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Drop lease for Child 2, Parent should have required level 0.
        // Grandparent should still have required level 90.
        drop(lease_child_2);
        let mut grandparent_req_level_fut = grandparent_required.watch();
        assert_eq!(
            executor.run_until_stalled(&mut grandparent_req_level_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(parent_req_level_fut.await.unwrap(), Ok(0));
        });
        let mut parent_req_level_fut = parent_required.watch();

        // Lower Parent's current level to 0. Grandparent claim should now be
        // dropped and have its default required level of 10.
        executor.run_singlethreaded(async {
            parent_current.update(0).await.unwrap().expect("update_current_power_level failed");
            assert_eq!(grandparent_req_level_fut.await.unwrap(), Ok(10));
        });
        assert_eq!(
            executor.run_until_stalled(&mut parent_req_level_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );

        Ok(())
    }

    #[test]
    fn test_passive() -> Result<()> {
        // B has an active dependency on A.
        // C has a passive dependency on B (and transitively, a passive dependency on A)
        //   and an active dependency on E.
        // D has an active dependency on B (and transitively, an active dependency on A).
        //  A     B     C     D     E
        // ON <= ON
        //       ON <- ON =======> ON
        //       ON <======= ON
        let mut executor = fasync::TestExecutor::new();
        let realm =
            executor.run_singlethreaded(async { build_power_broker_realm().await }).unwrap();
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
        let token_a = zx::Event::create();
        let (current_a, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (required_a, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let _element_a_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("A".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    active_dependency_tokens_to_register: Some(vec![token_a
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .unwrap()]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let token_b_active = zx::Event::create();
        let token_b_passive = zx::Event::create();
        let (current_b, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (required_b, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let _element_b_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("B".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: token_a.duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    }]),
                    active_dependency_tokens_to_register: Some(vec![token_b_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .unwrap()]),
                    passive_dependency_tokens_to_register: Some(vec![token_b_passive
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .unwrap()]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let token_e_active = zx::Event::create();
        let (current_e, current_server) = create_proxy::<CurrentLevelMarker>()?;
        let (required_e, required_server) = create_proxy::<RequiredLevelMarker>()?;
        let _element_e_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("E".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    active_dependency_tokens_to_register: Some(vec![token_e_active
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .unwrap()]),
                    level_control_channels: Some(fpb::LevelControlChannels {
                        current: current_server,
                        required: required_server,
                    }),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let (element_c_lessor, lessor_server) = create_proxy::<fpb::LessorMarker>()?;
        let _element_c_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("C".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    dependencies: Some(vec![
                        LevelDependency {
                            dependency_type: DependencyType::Passive,
                            dependent_level: BinaryPowerLevel::On.into_primitive(),
                            requires_token: token_b_passive
                                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                                .unwrap(),
                            requires_level: BinaryPowerLevel::On.into_primitive(),
                        },
                        LevelDependency {
                            dependency_type: DependencyType::Active,
                            dependent_level: BinaryPowerLevel::On.into_primitive(),
                            requires_token: token_e_active
                                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                                .unwrap(),
                            requires_level: BinaryPowerLevel::On.into_primitive(),
                        },
                    ]),
                    lessor_channel: Some(lessor_server),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });
        let (element_d_lessor, lessor_server) = create_proxy::<fpb::LessorMarker>()?;
        let _element_d_control = executor.run_singlethreaded(async {
            topology
                .add_element(ElementSchema {
                    element_name: Some("D".into()),
                    initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                    valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                    dependencies: Some(vec![LevelDependency {
                        dependency_type: DependencyType::Active,
                        dependent_level: BinaryPowerLevel::On.into_primitive(),
                        requires_token: token_b_active
                            .duplicate_handle(zx::Rights::SAME_RIGHTS)
                            .unwrap(),
                        requires_level: BinaryPowerLevel::On.into_primitive(),
                    }]),
                    lessor_channel: Some(lessor_server),
                    ..Default::default()
                })
                .await
                .unwrap()
                .expect("add_element failed")
        });

        // Initial required level for A, B & E should be OFF.
        // Set A, B & E's current level to OFF.
        executor.run_singlethreaded(async {
            let req_level_a = required_a.watch().await.unwrap();
            assert_eq!(req_level_a, Ok(BinaryPowerLevel::Off.into_primitive()));
            let req_level_b = required_b.watch().await.unwrap();
            assert_eq!(req_level_b, Ok(BinaryPowerLevel::Off.into_primitive()));
            let req_level_e = required_e.watch().await.unwrap();
            assert_eq!(req_level_e, Ok(BinaryPowerLevel::Off.into_primitive()));
            current_a
                .update(BinaryPowerLevel::Off.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
            current_b
                .update(BinaryPowerLevel::Off.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
            current_e
                .update(BinaryPowerLevel::Off.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        let mut required_a_fut = required_a.watch();
        let mut required_b_fut = required_b.watch();
        let mut required_e_fut = required_e.watch();

        // Lease C.
        // A and B's required levels should remain OFF because C's passive claim
        // does not raise the level of A or B.
        // E's required level should remain OFF because C's passive claim on B
        // has no other active claims to satisfy it (the lease is contingent)
        // and hence its active claim on E should remain pending and should not
        // raise the level of E.
        // Lease C should be Pending.
        let lease_c = executor.run_singlethreaded(async {
            element_c_lessor
                .lease(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("Lease response not ok")
                .into_proxy()
        })?;
        assert_eq!(
            executor.run_until_stalled(&mut required_a_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut required_b_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut required_e_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_c.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Lease D.
        // A should have required level ON because of D's transitive active claim.
        // B should still have required level OFF because A is not yet ON.
        // E should have required level ON because it C's lease is no longer
        // contingent on an active claim that would satisfy its passive claim.
        // Lease C & D should become pending.
        let lease_d = executor.run_singlethreaded(async {
            element_d_lessor
                .lease(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("Lease response not ok")
                .into_proxy()
        })?;
        executor.run_singlethreaded(async {
            assert_eq!(required_a_fut.await.unwrap(), Ok(BinaryPowerLevel::On.into_primitive()));
        });
        let mut required_a_fut = required_a.watch();
        assert_eq!(
            executor.run_until_stalled(&mut required_b_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(required_e_fut.await.unwrap(), Ok(BinaryPowerLevel::On.into_primitive()));
        });
        let mut required_e_fut = required_e.watch();
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_c.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
            assert_eq!(
                lease_d.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Update A's current level to ON.
        // A should still have required level ON.
        // B should now have required level ON because of D's active claim and
        // its dependency on A being satisfied.
        // E should still have required level ON.
        // Lease C & D should remain pending.
        executor.run_singlethreaded(async {
            current_a
                .update(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
        });
        assert_eq!(
            executor.run_until_stalled(&mut required_a_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(required_b_fut.await.unwrap(), Ok(BinaryPowerLevel::On.into_primitive()));
            assert_eq!(
                lease_c.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
            assert_eq!(
                lease_d.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });
        let mut required_b_fut = required_b.watch();
        assert_eq!(
            executor.run_until_stalled(&mut required_e_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );

        // Update B's current level to ON.
        // Lease D should become satisfied.
        executor.run_singlethreaded(async {
            current_b
                .update(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
            assert_eq!(
                lease_c.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
            assert_eq!(
                lease_d.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Update E's current level to ON.
        // Lease C should become satisfied.
        executor.run_singlethreaded(async {
            current_e
                .update(BinaryPowerLevel::On.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
            assert_eq!(
                lease_c.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
            assert_eq!(
                lease_d.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Satisfied
            );
        });

        // Drop Lease on D.
        // Lease C should now be Pending.
        // A, B & E's required level should remain ON.
        drop(lease_d);
        assert_eq!(
            executor.run_until_stalled(&mut required_a_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut required_b_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut required_e_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(
                lease_c.watch_status(LeaseStatus::Unknown).await.unwrap(),
                LeaseStatus::Pending
            );
        });

        // Drop Lease on C.
        // A's required level should remain ON.
        // B & E's required levels should become OFF.
        drop(lease_c);
        assert_eq!(
            executor.run_until_stalled(&mut required_a_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        executor.run_singlethreaded(async {
            assert_eq!(required_b_fut.await.unwrap(), Ok(BinaryPowerLevel::Off.into_primitive()));
        });
        let mut required_b_fut = required_b.watch();
        executor.run_singlethreaded(async {
            assert_eq!(required_e_fut.await.unwrap(), Ok(BinaryPowerLevel::Off.into_primitive()));
        });
        let mut required_e_fut = required_e.watch();

        // Update B's current level to OFF.
        // A's required level should become OFF.
        // B & E's required levels should remain OFF.
        executor.run_singlethreaded(async {
            current_b
                .update(BinaryPowerLevel::Off.into_primitive())
                .await
                .unwrap()
                .expect("update_current_power_level failed");
            assert_eq!(required_a_fut.await.unwrap(), Ok(BinaryPowerLevel::Off.into_primitive()));
        });
        assert_eq!(
            executor.run_until_stalled(&mut required_b_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );
        assert_eq!(
            executor.run_until_stalled(&mut required_e_fut).map(|fidl| fidl.unwrap()),
            Poll::Pending
        );

        Ok(())
    }

    #[fuchsia::test]
    async fn test_topology() -> Result<(), Error> {
        let realm = build_power_broker_realm().await?;

        // Create a four element topology.
        let topology = realm.root.connect_to_protocol_at_exposed_dir::<TopologyMarker>()?;
        let earth_token = zx::Event::create();
        let earth_element_control = topology
            .add_element(ElementSchema {
                element_name: Some("Earth".into()),
                initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                active_dependency_tokens_to_register: Some(vec![
                    earth_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?
                ]),
                ..Default::default()
            })
            .await?
            .expect("add_element failed");
        let earth_element_control = earth_element_control.into_proxy()?;
        let water_token = zx::Event::create();
        let water_element_control = topology
            .add_element(ElementSchema {
                element_name: Some("Water".into()),
                initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                dependencies: Some(vec![LevelDependency {
                    dependency_type: DependencyType::Active,
                    dependent_level: BinaryPowerLevel::On.into_primitive(),
                    requires_token: earth_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)
                        .expect("dup failed"),
                    requires_level: BinaryPowerLevel::On.into_primitive(),
                }]),
                active_dependency_tokens_to_register: Some(vec![
                    water_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?
                ]),
                ..Default::default()
            })
            .await?
            .expect("add_element failed");
        let water_element_control = water_element_control.into_proxy()?;
        let fire_token = zx::Event::create();
        let fire_element_control = topology
            .add_element(ElementSchema {
                element_name: Some("Fire".into()),
                initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                active_dependency_tokens_to_register: Some(vec![
                    fire_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?
                ]),
                ..Default::default()
            })
            .await?
            .expect("add_element failed");
        let fire_element_control = fire_element_control.into_proxy()?;
        let air_token = zx::Event::create();
        let air_element_control = topology
            .add_element(ElementSchema {
                element_name: Some("Air".into()),
                initial_current_level: Some(BinaryPowerLevel::Off.into_primitive()),
                valid_levels: Some(BINARY_POWER_LEVELS.to_vec()),
                active_dependency_tokens_to_register: Some(vec![
                    air_token.duplicate_handle(zx::Rights::SAME_RIGHTS)?
                ]),
                ..Default::default()
            })
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

        drop(air_element_control);

        // Confirm the element has been removed and Status channels have been
        // closed.
        let fire_status = {
            let (client, server) = create_proxy::<StatusMarker>()?;
            fire_element_control.open_status_channel(server)?;
            client
        };
        drop(fire_element_control);
        fire_status.as_channel().on_closed().await?;

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
}
