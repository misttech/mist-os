// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_utils::hanging_get::server::HangingGet;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_power_broker::{self as fbroker, LeaseStatus};
use fidl_fuchsia_power_suspend as fsuspend;
use fidl_fuchsia_power_system as fsystem;
use fidl_fuchsia_power_system::{
    ApplicationActivityLevel, ExecutionStateLevel, FullWakeHandlingLevel, WakeHandlingLevel,
};
use fidl_test_sagcontrol as fctrl;
use fidl_test_suspendcontrol as tsc;
use fuchsia_async as fasync;
use fuchsia_component::client as fclient;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::{ServiceFs, ServiceObjLocal};
use futures::{lock::Mutex, prelude::*};
use power_broker_client::PowerElementContext;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use tracing::error;

// TODO(b/336692041): Set up a more complex topology to allow fake SAG to override power element
// states.

type NotifyFn =
    Box<dyn Fn(&fctrl::SystemActivityGovernorState, fctrl::StateWatchResponder) -> bool>;
type StateHangingGet =
    HangingGet<fctrl::SystemActivityGovernorState, fctrl::StateWatchResponder, NotifyFn>;

async fn lease(controller: &PowerElementContext, level: u8) -> Result<fbroker::LeaseControlProxy> {
    let lease_control = controller
        .lessor
        .lease(level)
        .await?
        .map_err(|e| anyhow::anyhow!("{e:?}"))?
        .into_proxy()?;

    let mut lease_status = LeaseStatus::Unknown;
    while lease_status != LeaseStatus::Satisfied {
        lease_status = lease_control.watch_status(lease_status).await.unwrap();
    }

    Ok(lease_control)
}

pub struct SystemActivityGovernorControl {
    application_activity_controller: PowerElementContext,
    full_wake_handling_controller: PowerElementContext,
    wake_handling_controller: PowerElementContext,

    hanging_get: RefCell<StateHangingGet>,

    application_activity_lease: RefCell<Option<fbroker::LeaseControlProxy>>,
    wake_handling_lease: RefCell<Option<fbroker::LeaseControlProxy>>,
    full_wake_handling_lease: RefCell<Option<fbroker::LeaseControlProxy>>,

    boot_complete: Rc<Mutex<bool>>,
    current_state: Rc<Mutex<fctrl::SystemActivityGovernorState>>,
    required_state: Rc<Mutex<fctrl::SystemActivityGovernorState>>,
}

impl SystemActivityGovernorControl {
    pub async fn new(suspend_device: tsc::DeviceProxy) -> Rc<Self> {
        let topology = connect_to_protocol::<fbroker::TopologyMarker>().unwrap();
        let sag = connect_to_protocol::<fsystem::ActivityGovernorMarker>().unwrap();
        let sag_power_elements = sag.get_power_elements().await.unwrap();

        let wh_token = sag_power_elements.wake_handling.unwrap().active_dependency_token.unwrap();
        let wake_handling_controller =
            PowerElementContext::builder(&topology, "wake_controller", &[0, 1])
                .dependencies(vec![fbroker::LevelDependency {
                    dependency_type: fbroker::DependencyType::Active,
                    dependent_level: 1,
                    requires_token: wh_token,
                    requires_level: 1,
                }])
                .build()
                .await
                .unwrap();

        let fwh_token =
            sag_power_elements.full_wake_handling.unwrap().active_dependency_token.unwrap();
        let full_wake_handling_controller =
            PowerElementContext::builder(&topology, "full_wake_controller", &[0, 1])
                .dependencies(vec![fbroker::LevelDependency {
                    dependency_type: fbroker::DependencyType::Active,
                    dependent_level: 1,
                    requires_token: fwh_token,
                    requires_level: 1,
                }])
                .build()
                .await
                .unwrap();

        let aa_token =
            sag_power_elements.application_activity.unwrap().active_dependency_token.unwrap();
        let application_activity_controller =
            PowerElementContext::builder(&topology, "application_activity_controller", &[0, 1])
                .dependencies(vec![fbroker::LevelDependency {
                    dependency_type: fbroker::DependencyType::Active,
                    dependent_level: 1,
                    requires_token: aa_token,
                    requires_level: 1,
                }])
                .build()
                .await
                .unwrap();

        let boot_complete = Rc::new(Mutex::new(false));

        let element_info_provider = fclient::connect_to_service_instance::<
            fbroker::ElementInfoProviderServiceMarker,
        >(&"system_activity_governor")
        .expect("failed to connect to service ElementInfoProviderService")
        .connect_to_status_provider()
        .expect("failed to connect to protocol ElementInfoProvider");

        let mut status_endpoints: HashMap<String, fbroker::StatusProxy> = element_info_provider
            .get_status_endpoints()
            .await
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|s| (s.identifier.unwrap(), s.status.unwrap().into_proxy().unwrap()))
            .collect();

        let es_status = status_endpoints.remove("execution_state".into()).unwrap();
        let initial_execution_state_level = ExecutionStateLevel::from_primitive(
            es_status.watch_power_level().await.unwrap().unwrap(),
        )
        .unwrap();

        let aa_status = status_endpoints.remove("application_activity".into()).unwrap();
        let initial_application_activity_level = ApplicationActivityLevel::from_primitive(
            aa_status.watch_power_level().await.unwrap().unwrap(),
        )
        .unwrap();

        let fwh_status = status_endpoints.remove("full_wake_handling".into()).unwrap();
        let initial_full_wake_handling_level = FullWakeHandlingLevel::from_primitive(
            fwh_status.watch_power_level().await.unwrap().unwrap(),
        )
        .unwrap();

        let wh_status = status_endpoints.remove("wake_handling".into()).unwrap();
        let initial_wake_handling_level = WakeHandlingLevel::from_primitive(
            wh_status.watch_power_level().await.unwrap().unwrap(),
        )
        .unwrap();

        let state = fctrl::SystemActivityGovernorState {
            execution_state_level: Some(initial_execution_state_level),
            application_activity_level: Some(initial_application_activity_level),
            full_wake_handling_level: Some(initial_full_wake_handling_level),
            wake_handling_level: Some(initial_wake_handling_level),
            ..Default::default()
        };
        let current_state = Rc::new(Mutex::new(state.clone()));
        let required_state = Rc::new(Mutex::new(state.clone()));

        let hanging_get = StateHangingGet::new(
            state,
            Box::new(
                |state: &fctrl::SystemActivityGovernorState,
                 res: fctrl::StateWatchResponder|
                 -> bool {
                    if let Err(error) = res.send(state) {
                        tracing::warn!(?error, "Failed to send SAG state to client");
                    }
                    true
                },
            ),
        );

        let state_publisher = Rc::new(hanging_get.new_publisher());

        let publisher = state_publisher.clone();
        let current_state_clone = current_state.clone();
        let required_state_clone = required_state.clone();
        fasync::Task::local(async move {
            loop {
                let new_status = ExecutionStateLevel::from_primitive(
                    es_status.watch_power_level().await.unwrap().unwrap(),
                )
                .unwrap();
                current_state_clone.lock().await.execution_state_level.replace(new_status);
                let state = current_state_clone.lock().await.clone();
                if state == *required_state_clone.lock().await {
                    publisher.set(state);
                }

                if new_status == ExecutionStateLevel::Inactive {
                    assert_eq!(
                        0,
                        suspend_device.await_suspend().await.unwrap().unwrap().state_index.unwrap()
                    );
                    suspend_device
                        .resume(&tsc::DeviceResumeRequest::Result(tsc::SuspendResult {
                            suspend_duration: Some(1i64),
                            suspend_overhead: Some(1i64),
                            ..Default::default()
                        }))
                        .await
                        .unwrap()
                        .unwrap();
                }
            }
        })
        .detach();

        let boot_complete_clone = boot_complete.clone();
        let publisher = state_publisher.clone();
        let current_state_clone = current_state.clone();
        let required_state_clone = required_state.clone();
        fasync::Task::local(async move {
            loop {
                let new_status = ApplicationActivityLevel::from_primitive(
                    aa_status.watch_power_level().await.unwrap().unwrap(),
                )
                .unwrap();
                current_state_clone.lock().await.application_activity_level.replace(new_status);
                let state = current_state_clone.lock().await.clone();
                if state == *required_state_clone.lock().await {
                    publisher.set(state);
                }

                if new_status == ApplicationActivityLevel::Active
                    && *boot_complete_clone.lock().await == false
                {
                    *boot_complete_clone.lock().await = true;
                }
            }
        })
        .detach();

        let publisher = state_publisher.clone();
        let current_state_clone = current_state.clone();
        let required_state_clone = required_state.clone();
        fasync::Task::local(async move {
            loop {
                let new_status = FullWakeHandlingLevel::from_primitive(
                    fwh_status.watch_power_level().await.unwrap().unwrap(),
                )
                .unwrap();
                current_state_clone.lock().await.full_wake_handling_level.replace(new_status);
                let state = current_state_clone.lock().await.clone();
                if state == *required_state_clone.lock().await {
                    publisher.set(state);
                }
            }
        })
        .detach();

        let publisher = state_publisher.clone();
        let current_state_clone = current_state.clone();
        let required_state_clone = required_state.clone();
        fasync::Task::local(async move {
            loop {
                let new_status = WakeHandlingLevel::from_primitive(
                    wh_status.watch_power_level().await.unwrap().unwrap(),
                )
                .unwrap();
                current_state_clone.lock().await.wake_handling_level.replace(new_status);
                let state = current_state_clone.lock().await.clone();
                if state == *required_state_clone.lock().await {
                    publisher.set(state);
                }
            }
        })
        .detach();

        Rc::new(Self {
            application_activity_controller,
            full_wake_handling_controller,
            wake_handling_controller,
            hanging_get: RefCell::new(hanging_get),
            application_activity_lease: RefCell::new(None),
            wake_handling_lease: RefCell::new(None),
            full_wake_handling_lease: RefCell::new(None),
            boot_complete,
            current_state,
            required_state,
        })
    }

    pub async fn run(self: Rc<Self>, fs: &mut ServiceFs<ServiceObjLocal<'_, ()>>) {
        let this = self.clone();
        fs.dir("svc")
            .add_fidl_service(move |mut stream: fctrl::StateRequestStream| {
                let this = this.clone();
                fasync::Task::local(async move {
                    let sub = this.hanging_get.borrow_mut().new_subscriber();
                    while let Ok(Some(request)) = stream.try_next().await {
                        match request {
                            fctrl::StateRequest::Set { responder, payload } => {
                                let result = this.update_sag_state(payload).await;
                                let _ = responder.send(result);
                            }
                            fctrl::StateRequest::Get { responder } => {
                                let _ = responder.send(&*this.current_state.lock().await);
                            }
                            fctrl::StateRequest::Watch { responder } => {
                                if let Err(error) = sub.register(responder) {
                                    tracing::warn!(?error, "Failed to register for Watch call");
                                }
                            }
                            fctrl::StateRequest::_UnknownMethod { ordinal, .. } => {
                                tracing::warn!(?ordinal, "Unknown StateRequest method");
                            }
                        }
                    }
                })
                .detach();
            })
            .add_service_connector(move |server_end: ServerEnd<fsuspend::StatsMarker>| {
                fclient::connect_channel_to_protocol::<fsuspend::StatsMarker>(
                    server_end.into_channel(),
                )
                .unwrap();
            })
            .add_service_connector(
                move |server_end: ServerEnd<fsystem::ActivityGovernorMarker>| {
                    fclient::connect_channel_to_protocol::<fsystem::ActivityGovernorMarker>(
                        server_end.into_channel(),
                    )
                    .unwrap();
                },
            );
    }

    async fn handle_full_partial_wake_handling_changes(
        self: &Rc<Self>,
        required_full_wake_handling_level: FullWakeHandlingLevel,
        required_wake_handling_level: WakeHandlingLevel,
    ) -> Result<()> {
        // Take any wake_handling/full_wake_handling lease before dropping any to prevent a suspend
        // in between.
        // Only take a new lease if it doesn't already exist.
        if required_full_wake_handling_level == FullWakeHandlingLevel::Active {
            let _ = self
                .full_wake_handling_lease
                .borrow_mut()
                .get_or_insert(lease(&self.full_wake_handling_controller, 1).await?);
        }
        if required_wake_handling_level == WakeHandlingLevel::Active {
            let _ = self
                .wake_handling_lease
                .borrow_mut()
                .get_or_insert(lease(&self.wake_handling_controller, 1).await?);
        }
        if required_wake_handling_level == WakeHandlingLevel::Inactive {
            drop(self.wake_handling_lease.borrow_mut().take());
        }
        if required_full_wake_handling_level == FullWakeHandlingLevel::Inactive {
            drop(self.full_wake_handling_lease.borrow_mut().take());
        }
        Ok(())
    }

    async fn handle_application_activity_changes(
        self: &Rc<Self>,
        required_application_activity_level: ApplicationActivityLevel,
    ) -> Result<()> {
        match required_application_activity_level {
            ApplicationActivityLevel::Active => {
                let _ = self
                    .application_activity_lease
                    .borrow_mut()
                    .get_or_insert(lease(&self.application_activity_controller, 1).await?);
            }
            ApplicationActivityLevel::Inactive => {
                drop(self.application_activity_lease.borrow_mut().take());
            }
            _ => (),
        }
        Ok(())
    }

    async fn update_sag_state(
        self: &Rc<Self>,
        sag_state: fctrl::SystemActivityGovernorState,
    ) -> fctrl::StateSetResult {
        let required_execution_state_level = sag_state
            .execution_state_level
            .unwrap_or(self.current_state.lock().await.execution_state_level.unwrap());
        let required_application_activity_level = sag_state
            .application_activity_level
            .unwrap_or(self.current_state.lock().await.application_activity_level.unwrap());
        let required_full_wake_handling_level = sag_state
            .full_wake_handling_level
            .unwrap_or(self.current_state.lock().await.full_wake_handling_level.unwrap());
        let required_wake_handling_level = sag_state
            .wake_handling_level
            .unwrap_or(self.current_state.lock().await.wake_handling_level.unwrap());
        self.required_state
            .lock()
            .await
            .execution_state_level
            .replace(required_execution_state_level);
        self.required_state
            .lock()
            .await
            .application_activity_level
            .replace(required_application_activity_level);
        self.required_state
            .lock()
            .await
            .full_wake_handling_level
            .replace(required_full_wake_handling_level);
        self.required_state.lock().await.wake_handling_level.replace(required_wake_handling_level);

        match required_execution_state_level {
            ExecutionStateLevel::Inactive => {
                if *self.boot_complete.lock().await == false
                    || required_application_activity_level != ApplicationActivityLevel::Inactive
                    || required_full_wake_handling_level != FullWakeHandlingLevel::Inactive
                    || required_wake_handling_level != WakeHandlingLevel::Inactive
                {
                    return Err(fctrl::SetSystemActivityGovernorStateError::NotSupported);
                }
                self.handle_application_activity_changes(required_application_activity_level)
                    .await
                    .map_err(|err| {
                        error!(%err, "Request failed with internal error");
                        fctrl::SetSystemActivityGovernorStateError::Internal
                    })?;
                self.handle_full_partial_wake_handling_changes(
                    required_full_wake_handling_level,
                    required_wake_handling_level,
                )
                .await
                .map_err(|err| {
                    error!(%err, "Request failed with internal error");
                    fctrl::SetSystemActivityGovernorStateError::Internal
                })?;
            }
            ExecutionStateLevel::WakeHandling => {
                if *self.boot_complete.lock().await == false
                    || required_application_activity_level != ApplicationActivityLevel::Inactive
                    || (required_full_wake_handling_level == FullWakeHandlingLevel::Inactive
                        && required_wake_handling_level == WakeHandlingLevel::Inactive)
                {
                    return Err(fctrl::SetSystemActivityGovernorStateError::NotSupported);
                }

                // Take any wake handling leases before dropping any application activity lease.
                self.handle_full_partial_wake_handling_changes(
                    required_full_wake_handling_level,
                    required_wake_handling_level,
                )
                .await
                .map_err(|err| {
                    error!(%err, "Request failed with internal error");
                    fctrl::SetSystemActivityGovernorStateError::Internal
                })?;

                self.handle_application_activity_changes(required_application_activity_level)
                    .await
                    .map_err(|err| {
                        error!(%err, "Request failed with internal error");
                        fctrl::SetSystemActivityGovernorStateError::Internal
                    })?;
            }
            ExecutionStateLevel::Active => {
                if *self.boot_complete.lock().await == true
                    && required_application_activity_level != ApplicationActivityLevel::Active
                {
                    return Err(fctrl::SetSystemActivityGovernorStateError::NotSupported);
                }

                // Take any application activity lease before dropping any wake handling leases.
                self.handle_application_activity_changes(required_application_activity_level)
                    .await
                    .map_err(|err| {
                        error!(%err, "Request failed with internal error");
                        fctrl::SetSystemActivityGovernorStateError::Internal
                    })?;

                self.handle_full_partial_wake_handling_changes(
                    required_full_wake_handling_level,
                    required_wake_handling_level,
                )
                .await
                .map_err(|err| {
                    error!(%err, "Request failed with internal error");
                    fctrl::SetSystemActivityGovernorStateError::Internal
                })?;
            }
            _ => (),
        }
        Ok(())
    }
}
