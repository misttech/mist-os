// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_power_broker::{self as fbroker, LeaseStatus};
use fidl_fuchsia_power_system::{self as fsystem, ApplicationActivityLevel, ExecutionStateLevel};
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use futures::channel::mpsc;
use futures::StreamExt;
use power_broker_client::PowerElementContext;
use tracing::*;
use {fidl_test_sagcontrol as fctrl, fuchsia_async as fasync};

struct TestEnv {
    realm_instance: RealmInstance,
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
        .add_child(
            "fake-system-activity-governor",
            "fake-system-activity-governor#meta/fake-system-activity-governor.cm",
            ChildOptions::new(),
        )
        .await
        .expect("Failed to add child: fake-system-activity-governor");

    let power_broker_ref = builder
        .add_child("power-broker", "#meta/power-broker.cm", ChildOptions::new())
        .await
        .expect("Failed to add child: power-broker");

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

    // Expose config capabilities to system-activity-governor.
    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.power.UseSuspender".parse().unwrap(),
            value: false.into(),
        }))
        .await
        .unwrap();

    builder
        .add_route(
            Route::new()
                .capability(Capability::configuration("fuchsia.power.UseSuspender"))
                .from(Ref::self_())
                .to(&component_ref),
        )
        .await
        .unwrap();

    builder
        .add_route(
            Route::new()
                .capability(Capability::configuration("fuchsia.power.WaitForSuspendingToken"))
                .from(Ref::void())
                .to(&component_ref),
        )
        .await
        .unwrap();

    // Expose capabilities from power-broker to fake-system-activity-governor.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.broker.Topology"))
                .from(&power_broker_ref)
                .to(&component_ref),
        )
        .await
        .unwrap();

    // Expose capabilities from fake-system-activity-governor.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name(
                    "fuchsia.power.broker.ElementInfoProvider",
                ))
                .capability(Capability::protocol_by_name("test.sagcontrol.State"))
                .capability(Capability::protocol_by_name("fuchsia.power.suspend.Stats"))
                .capability(Capability::protocol_by_name("fuchsia.power.system.ActivityGovernor"))
                .from(&component_ref)
                .to(Ref::parent()),
        )
        .await
        .unwrap();

    let realm_instance = builder.build().await.expect("Failed to build RealmInstance");
    TestEnv { realm_instance }
}

#[fuchsia::test]
async fn test_fsystem_activity_governor_listener_and_get_power_element() -> Result<()> {
    let env = create_test_env().await;

    let activity_governor = env.connect_to_protocol::<fsystem::ActivityGovernorMarker>();
    let sag_ctrl_state = env.connect_to_protocol::<fctrl::StateMarker>();
    let topology = env.connect_to_protocol::<fbroker::TopologyMarker>();

    // Check initial booting state [2, 0, 0, 0].
    assert_eq!(
        sag_ctrl_state.watch().await.unwrap(),
        fctrl::SystemActivityGovernorState {
            execution_state_level: Some(ExecutionStateLevel::Active),
            application_activity_level: Some(ApplicationActivityLevel::Inactive),
            ..Default::default()
        }
    );

    let power_elements = activity_governor.get_power_elements().await?;
    let es_token = power_elements.execution_state.unwrap().opportunistic_dependency_token.unwrap();

    let test_driver = PowerElementContext::builder(&topology, "test_driver", &[0, 1])
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Opportunistic,
            dependent_level: 1,
            requires_token: es_token,
            requires_level_by_preference: vec![2],
        }])
        .build()
        .await?;
    assert_eq!(0, test_driver.required_level.watch().await?.unwrap());

    let test_driver_controller =
        PowerElementContext::builder(&topology, "test_driver_controller", &[0, 1])
            .dependencies(vec![fbroker::LevelDependency {
                dependency_type: fbroker::DependencyType::Assertive,
                dependent_level: 1,
                requires_token: test_driver.assertive_dependency_token().unwrap(),
                requires_level_by_preference: vec![1],
            }])
            .build()
            .await?;

    let (listener_client_end, mut listener_stream) =
        fidl::endpoints::create_request_stream().unwrap();
    activity_governor
        .register_listener(fsystem::ActivityGovernorRegisterListenerRequest {
            listener: Some(listener_client_end),
            ..Default::default()
        })
        .await
        .unwrap();

    let (on_suspend_started_tx, mut on_suspend_started_rx) = mpsc::channel(1);
    let (on_resume_tx, mut on_resume_rx) = mpsc::channel(1);

    fasync::Task::local(async move {
        let mut on_suspend_started_tx = on_suspend_started_tx;
        let mut on_resume_tx = on_resume_tx;

        while let Some(Ok(req)) = listener_stream.next().await {
            match req {
                fsystem::ActivityGovernorListenerRequest::OnResume { responder } => {
                    responder.send().unwrap();
                    on_resume_tx.try_send(()).unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendStarted { responder } => {
                    responder.send().unwrap();
                    on_suspend_started_tx.try_send(()).unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::OnSuspendFail { responder } => {
                    responder.send().unwrap();
                }
                fsystem::ActivityGovernorListenerRequest::_UnknownMethod { ordinal, .. } => {
                    panic!("Unexpected method: {}", ordinal);
                }
            }
        }
    })
    .detach();

    // Trigger "boot complete" logic and a suspend/resume cycle.
    let _ = sag_ctrl_state
        .set(&fctrl::SystemActivityGovernorState {
            application_activity_level: Some(ApplicationActivityLevel::Active),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    let mut current_state = fctrl::SystemActivityGovernorState {
        execution_state_level: Some(ExecutionStateLevel::Active),
        application_activity_level: Some(ApplicationActivityLevel::Active),
        ..Default::default()
    };
    assert_eq!(sag_ctrl_state.watch().await.unwrap(), current_state);

    let _ = sag_ctrl_state
        .set(&fctrl::SystemActivityGovernorState {
            execution_state_level: Some(ExecutionStateLevel::Inactive),
            application_activity_level: Some(ApplicationActivityLevel::Inactive),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();
    current_state.application_activity_level.replace(ApplicationActivityLevel::Inactive);
    current_state.execution_state_level.replace(ExecutionStateLevel::Inactive);
    assert_eq!(sag_ctrl_state.watch().await.unwrap(), current_state);

    // OnSuspendStarted and OnResume should have been called once.
    on_suspend_started_rx.next().await.unwrap();
    on_resume_rx.next().await.unwrap();

    let lease_control = test_driver_controller
        .lessor
        .lease(1)
        .await?
        .map_err(|e| anyhow::anyhow!("{e:?}"))?
        .into_proxy();

    assert_eq!(
        LeaseStatus::Pending,
        lease_control.watch_status(LeaseStatus::Unknown).await.unwrap()
    );

    let _ = sag_ctrl_state
        .set(&fctrl::SystemActivityGovernorState {
            execution_state_level: Some(ExecutionStateLevel::Active),
            application_activity_level: Some(ApplicationActivityLevel::Active),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();
    current_state.application_activity_level.replace(ApplicationActivityLevel::Active);
    current_state.execution_state_level.replace(ExecutionStateLevel::Active);
    assert_eq!(sag_ctrl_state.watch().await.unwrap(), current_state);

    assert_eq!(1, test_driver.required_level.watch().await?.unwrap());
    assert_eq!(
        LeaseStatus::Pending,
        lease_control.watch_status(LeaseStatus::Unknown).await.unwrap()
    );
    test_driver.current_level.update(1).await?.unwrap();
    assert_eq!(
        LeaseStatus::Pending,
        lease_control.watch_status(LeaseStatus::Unknown).await.unwrap()
    );
    test_driver_controller.current_level.update(1).await?.unwrap();
    assert_eq!(
        LeaseStatus::Satisfied,
        lease_control.watch_status(LeaseStatus::Unknown).await.unwrap()
    );

    // TODO(didis): Add test for setting ExecutionStateLevel to Inactive after
    // fxr/1014552 lands.

    Ok(())
}

#[fuchsia::test]
async fn test_set_valid_sag_states() -> Result<()> {
    let env = create_test_env().await;

    let sag_ctrl_state = env.connect_to_protocol::<fctrl::StateMarker>();

    // Check initial booting state [2, 0, 0].
    let mut current_state = fctrl::SystemActivityGovernorState {
        execution_state_level: Some(ExecutionStateLevel::Active),
        application_activity_level: Some(ApplicationActivityLevel::Inactive),
        ..Default::default()
    };

    assert_eq!(sag_ctrl_state.watch().await.unwrap(), current_state);

    // Trigger "boot complete" logic.
    let _ = sag_ctrl_state
        .set(&fctrl::SystemActivityGovernorState {
            application_activity_level: Some(ApplicationActivityLevel::Active),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    // Wait until SAG state changes to [2, 1, 0].
    current_state.application_activity_level.replace(ApplicationActivityLevel::Active);
    assert_eq!(sag_ctrl_state.watch().await.unwrap(), current_state);

    let _ = sag_ctrl_state
        .set(&fctrl::SystemActivityGovernorState {
            execution_state_level: Some(ExecutionStateLevel::Suspending),
            application_activity_level: Some(ApplicationActivityLevel::Inactive),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    // Wait until SAG state changes to [1, 0, 0].
    current_state.execution_state_level.replace(ExecutionStateLevel::Suspending);
    current_state.application_activity_level.replace(ApplicationActivityLevel::Inactive);
    assert_eq!(sag_ctrl_state.watch().await.unwrap(), current_state);

    let _ = sag_ctrl_state
        .set(&fctrl::SystemActivityGovernorState {
            execution_state_level: Some(ExecutionStateLevel::Active),
            application_activity_level: Some(ApplicationActivityLevel::Active),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    // Wait until SAG state changes to [2, 1, 0].
    current_state.execution_state_level.replace(ExecutionStateLevel::Active);
    current_state.application_activity_level.replace(ApplicationActivityLevel::Active);
    assert_eq!(sag_ctrl_state.watch().await.unwrap(), current_state);

    let _ = sag_ctrl_state
        .set(&fctrl::SystemActivityGovernorState {
            execution_state_level: Some(ExecutionStateLevel::Inactive),
            application_activity_level: Some(ApplicationActivityLevel::Inactive),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    // Wait until SAG state changes to [0, 0, 0].
    current_state.execution_state_level.replace(ExecutionStateLevel::Inactive);
    current_state.application_activity_level.replace(ApplicationActivityLevel::Inactive);
    assert_eq!(sag_ctrl_state.watch().await.unwrap(), current_state);

    let _ = sag_ctrl_state
        .set(&fctrl::SystemActivityGovernorState {
            execution_state_level: Some(ExecutionStateLevel::Active),
            application_activity_level: Some(ApplicationActivityLevel::Active),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    // Wait until SAG state changes to [2, 1, 0].
    current_state.execution_state_level.replace(ExecutionStateLevel::Active);
    current_state.application_activity_level.replace(ApplicationActivityLevel::Active);
    assert_eq!(sag_ctrl_state.watch().await.unwrap(), current_state);

    let _ = sag_ctrl_state
        .set(&fctrl::SystemActivityGovernorState {
            execution_state_level: Some(ExecutionStateLevel::Suspending),
            application_activity_level: Some(ApplicationActivityLevel::Inactive),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();

    // Wait until SAG state changes to [1, 0, 0].
    current_state.execution_state_level.replace(ExecutionStateLevel::Suspending);
    current_state.application_activity_level.replace(ApplicationActivityLevel::Inactive);
    assert_eq!(sag_ctrl_state.watch().await.unwrap(), current_state);

    Ok(())
}

#[fuchsia::test]
async fn test_set_invalid_sag_states() -> Result<()> {
    let env = create_test_env().await;

    let sag_ctrl_state = env.connect_to_protocol::<fctrl::StateMarker>();

    // Check initial booting state [2, 0, 0].
    assert_eq!(
        sag_ctrl_state.watch().await.unwrap(),
        fctrl::SystemActivityGovernorState {
            execution_state_level: Some(ExecutionStateLevel::Active),
            application_activity_level: Some(ApplicationActivityLevel::Inactive),
            ..Default::default()
        }
    );

    let mut state = fctrl::SystemActivityGovernorState {
        execution_state_level: Some(ExecutionStateLevel::Active),
        application_activity_level: Some(ApplicationActivityLevel::Inactive),
        ..Default::default()
    };

    // Trigger "boot complete" logic.
    assert_eq!(
        sag_ctrl_state
            .set(&fctrl::SystemActivityGovernorState {
                application_activity_level: Some(ApplicationActivityLevel::Active),
                ..Default::default()
            },)
            .await
            .unwrap(),
        Ok(())
    );
    state.application_activity_level.replace(ApplicationActivityLevel::Active);
    assert_eq!(sag_ctrl_state.watch().await.unwrap(), state);

    // After triggering "boot complete" logic, when ExecutionState is Active, ApplicationActivity has to be active.
    assert_eq!(
        sag_ctrl_state
            .set(&fctrl::SystemActivityGovernorState {
                application_activity_level: Some(ApplicationActivityLevel::Inactive),
                ..Default::default()
            },)
            .await
            .unwrap(),
        Err(fctrl::SetSystemActivityGovernorStateError::NotSupported)
    );

    assert_eq!(
        sag_ctrl_state
            .set(&fctrl::SystemActivityGovernorState {
                execution_state_level: Some(ExecutionStateLevel::Active),
                application_activity_level: Some(ApplicationActivityLevel::Inactive),
                ..Default::default()
            },)
            .await
            .unwrap(),
        Err(fctrl::SetSystemActivityGovernorStateError::NotSupported)
    );

    // When ExecutionState is Inactive, everything else need to be inactive.
    assert_eq!(
        sag_ctrl_state
            .set(&fctrl::SystemActivityGovernorState {
                execution_state_level: Some(ExecutionStateLevel::Inactive),
                application_activity_level: Some(ApplicationActivityLevel::Active),
                ..Default::default()
            },)
            .await
            .unwrap(),
        Err(fctrl::SetSystemActivityGovernorStateError::NotSupported)
    );

    Ok(())
}
