// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::system_activity_governor::SystemActivityGovernor;
use anyhow::Result;
use async_lock::OnceCell;
use futures::future::LocalBoxFuture;
use futures::StreamExt;
use power_broker_client::PowerElementContext;
use std::rc::Rc;
use {fidl_fuchsia_power_broker as fbroker, fidl_fuchsia_power_system as fsystem};

/// SystemActivityGovernorFactory is a function trait used to construct a new
/// SystemActivityGovernor instance. The parameters are provided by an instance
/// of CpuElementManager.
pub trait SystemActivityGovernorFactory:
    Fn(
        /*cpu_power_element:*/ Rc<PowerElementContext>,
        /*execution_state_level_dependencies:*/ Vec<fbroker::LevelDependency>,
    ) -> LocalBoxFuture<'static, Result<Rc<SystemActivityGovernor>>>
    + 'static
{
}

// SystemActivityGovernorFactory should be implemented for all functions with
// a matching signature.
impl<T> SystemActivityGovernorFactory for T where
    T: Fn(
            /*cpu_power_element:*/ Rc<PowerElementContext>,
            /*execution_state_level_dependencies:*/ Vec<fbroker::LevelDependency>,
        ) -> LocalBoxFuture<'static, Result<Rc<SystemActivityGovernor>>>
        + 'static
{
}

pub struct CpuElementManager<F>
where
    F: SystemActivityGovernorFactory,
{
    cpu: Rc<PowerElementContext>,
    sag: OnceCell<Rc<SystemActivityGovernor>>,
    sag_factory: F,
}

impl<F> CpuElementManager<F>
where
    F: SystemActivityGovernorFactory,
{
    pub async fn new_wait_for_suspending_token(
        topology: &fbroker::TopologyProxy,
        sag_factory: F,
    ) -> Rc<Self> {
        let cpu = Rc::new(
            PowerElementContext::builder(
                topology,
                "cpu",
                &[
                    fsystem::CpuLevel::Inactive.into_primitive(),
                    fsystem::CpuLevel::Active.into_primitive(),
                ],
            )
            .build()
            .await
            .expect("PowerElementContext encountered error while building cpu"),
        );

        Rc::new(Self { cpu, sag: OnceCell::new(), sag_factory })
    }

    pub async fn new(topology: &fbroker::TopologyProxy, sag_factory: F) -> Rc<Self> {
        let manager = Self::new_wait_for_suspending_token(topology, sag_factory).await;

        // No dependencies will be provided, so construct SystemActivityGovernor now.
        let sag = (manager.sag_factory)(manager.cpu.clone(), Vec::new())
            .await
            .expect("create sag failed");
        let _ = manager.sag.set(sag).await;

        manager
    }

    pub async fn sag(&self) -> Rc<SystemActivityGovernor> {
        self.sag.wait().await.clone()
    }

    pub async fn handle_cpu_element_manager_stream(
        &self,
        mut stream: fsystem::CpuElementManagerRequestStream,
    ) {
        while let Some(request) = stream.next().await {
            match request {
                Ok(fsystem::CpuElementManagerRequest::GetCpuDependencyToken { responder }) => {
                    if let Err(error) = responder.send(fsystem::Cpu {
                        assertive_dependency_token: Some(
                            self.cpu.assertive_dependency_token().expect("token not available"),
                        ),
                        ..Default::default()
                    }) {
                        tracing::warn!(
                            ?error,
                            "Encountered error while responding to GetCpuDependencyToken request"
                        );
                    }
                }
                Ok(fsystem::CpuElementManagerRequest::AddExecutionStateDependency {
                    payload,
                    responder,
                }) => {
                    // To handle this request, both fields must be provided.
                    let response = match payload {
                        fsystem::CpuElementManagerAddExecutionStateDependencyRequest {
                            dependency_token: Some(dependency_token),
                            power_level: Some(power_level),
                            ..
                        } => {
                            // SAG can only be created once. If it already exists,
                            // no more dependencies can be added. Trying to add more
                            // dependencies after construction is an error and
                            // indicates a bug in the device's configuration.
                            if self.sag.is_initialized() {
                                tracing::error!("System Activity Governor is already created.");
                                Err(fsystem::AddExecutionStateDependencyError::BadState)
                            } else {
                                // We have a valid request to add an Execution State
                                // dependency, so we can construct SAG now.
                                tracing::info!("Adding execution state dependency");

                                let sag = (self.sag_factory)(
                                    self.cpu.clone(),
                                    vec![fbroker::LevelDependency {
                                        dependency_type: fbroker::DependencyType::Assertive,
                                        dependent_level: fsystem::ExecutionStateLevel::Suspending
                                            .into_primitive(),
                                        requires_token: dependency_token,
                                        requires_level_by_preference: vec![power_level],
                                    }],
                                )
                                .await
                                .expect("create sag failed");

                                // When sag is set, pending requests for other
                                // fuchsia.power.system FIDL protocols will be
                                // handled by it.
                                match self.sag.set(sag).await {
                                    Ok(_) => Ok(()),
                                    Err(_) => {
                                        tracing::error!(
                                            "System Activity Governor is already created."
                                        );
                                        Err(fsystem::AddExecutionStateDependencyError::BadState)
                                    }
                                }
                            }
                        }
                        payload => {
                            tracing::warn!(
                                "Invalid payload for AddExecutionStateDependency: {payload:?}"
                            );
                            Err(fsystem::AddExecutionStateDependencyError::InvalidArgs)
                        }
                    };

                    // At this point, SAG is constructed but may not be
                    // handling FIDL requests. Full initialization will
                    // occur asynchronously while responding to the caller.
                    if let Err(error) = responder.send(response) {
                        tracing::warn!(
                            ?error,
                            "Encountered error while responding to AddExecutionStateDependency request"
                        );
                    }
                }
                Ok(fsystem::CpuElementManagerRequest::_UnknownMethod { ordinal, .. }) => {
                    tracing::warn!(?ordinal, "Unknown CpuElementManagerRequest method");
                }
                Err(error) => {
                    tracing::error!(?error, "Error handling CpuElementManager request stream");
                }
            }
        }
    }
}
