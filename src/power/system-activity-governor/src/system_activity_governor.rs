// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use async_utils::hanging_get::server::{HangingGet, Publisher};
use fidl::endpoints::{create_endpoints, Proxy};
use fidl_fuchsia_hardware_suspend::{
    self as fhsuspend, SuspenderSuspendResponse as SuspendResponse,
};
use fidl_fuchsia_power_system::{
    self as fsystem, ApplicationActivityLevel, CpuLevel, ExecutionStateLevel, WakeHandlingLevel,
};
use fuchsia_inspect::{
    ArrayProperty, IntProperty as IInt, Node as INode, Property, UintProperty as IUint,
};
use fuchsia_inspect_contrib::nodes::{BoundedListNode as IRingBuffer, NodeTimeExt};
use futures::future::FutureExt;
use futures::lock::Mutex;
use futures::prelude::*;
use power_broker_client::{
    basic_update_fn_factory, run_power_element, LeaseHelper, PowerElementContext,
};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use zx::{AsHandleRef, HandleBased};
use {
    fidl_fuchsia_power_broker as fbroker, fidl_fuchsia_power_observability as fobs,
    fidl_fuchsia_power_suspend as fsuspend, fuchsia_async as fasync,
};

type NotifyFn = Box<dyn Fn(&fsuspend::SuspendStats, fsuspend::StatsWatchResponder) -> bool>;
type StatsHangingGet = HangingGet<fsuspend::SuspendStats, fsuspend::StatsWatchResponder, NotifyFn>;
type StatsPublisher = Publisher<fsuspend::SuspendStats, fsuspend::StatsWatchResponder, NotifyFn>;

#[derive(Copy, Clone)]
enum BootControlLevel {
    Inactive,
    Active,
}

/// The result of a suspend request.
#[derive(Debug, PartialEq)]
pub enum SuspendResult {
    /// Suspend request succeeded.
    Success,
    /// Suspend request was not allowed at the time it was triggered.
    NotAllowed,
    /// Suspend request failed.
    Fail,
}

impl From<BootControlLevel> for fbroker::PowerLevel {
    fn from(bc: BootControlLevel) -> Self {
        match bc {
            BootControlLevel::Inactive => 0,
            BootControlLevel::Active => 1,
        }
    }
}

#[async_trait(?Send)]
pub trait SuspendResumeListener {
    /// Gets the manager of suspend stats.
    fn suspend_stats(&self) -> &SuspendStatsManager;
    /// Leases (Execution State, Suspending). Called after system suspension ends.
    async fn on_suspend_ended(&self, suspend_suceeded: bool);
    /// Notify the listeners that system suspension is about to begin
    async fn notify_on_suspend(&self);
    /// Notify the listeners of suspend results.
    async fn notify_suspend_ended(&self);
    /// Notify the listeners of a suspend failure.
    async fn notify_on_suspend_fail(&self);
    /// Notify the listeners of a suspend success.
    async fn notify_on_resume(&self);
}

// TODO(https://fxbug.dev/372779207): Move CpuManager-related types out of this file for better
// organization and encapsulation. This will require refactoring/removing ExecutionResumeLatency.

/// Controls access to CPU power element and suspend management.
struct CpuManagerInner {
    /// The context used to manage the CPU power element.
    cpu: Rc<PowerElementContext>,
    /// The FIDL proxy to the device used to trigger system suspend.
    suspender: Option<fhsuspend::SuspenderProxy>,
    /// The suspend state index that will be passed to the suspender when system suspend is
    /// triggered.
    suspend_state_index: u64,
    /// The flag used to track whether suspension is allowed based on CPU's power level.
    /// If true, CPU has transitioned from a higher power state to CpuLevel::Inactive
    /// and is still at the CpuLevel::Inactive power level.
    suspend_allowed: bool,
}

/// Manager of the CPU power element and suspend logic.
pub struct CpuManager {
    /// State of the CPU power element and suspend controls.
    inner: Mutex<CpuManagerInner>,
    /// SuspendResumeListener object to notify of suspend/resume.
    suspend_resume_listener: OnceCell<Rc<dyn SuspendResumeListener>>,
    _inspect_node: RefCell<IRingBuffer>,
}

impl CpuManager {
    /// Creates a new CpuManager.
    pub fn new(
        cpu: Rc<PowerElementContext>,
        suspender: Option<fhsuspend::SuspenderProxy>,
        inspect: INode,
    ) -> Self {
        Self {
            inner: Mutex::new(CpuManagerInner {
                cpu,
                suspender,
                suspend_state_index: 0,
                suspend_allowed: false,
            }),
            suspend_resume_listener: OnceCell::new(),
            _inspect_node: RefCell::new(IRingBuffer::new(inspect, 128)),
        }
    }

    /// Sets the suspend resume listener.
    /// The listener can only be set once. Subsequent calls will result in a panic.
    pub fn set_suspend_resume_listener(
        &self,
        suspend_resume_listener: Rc<dyn SuspendResumeListener>,
    ) {
        self.suspend_resume_listener
            .set(suspend_resume_listener)
            .map_err(|_| anyhow::anyhow!("suspend_resume_listener is already set"))
            .unwrap();
    }

    /// Sets the suspend state index that will be used when suspend is triggered.
    async fn set_suspend_state_index(&self, suspend_state_index: u64) {
        tracing::debug!(?suspend_state_index, "set_suspend_state_index: acquiring inner lock");
        self.inner.lock().await.suspend_state_index = suspend_state_index;
    }

    /// Updates the power level of the CPU power element.
    ///
    /// Returns a Result that indicates whether the system should suspend or not.
    /// If an error occurs while updating the power level, the error is forwarded to the caller.
    pub async fn update_current_level(&self, required_level: fbroker::PowerLevel) -> Result<bool> {
        tracing::debug!(?required_level, "update_current_level: acquiring inner lock");
        let mut inner = self.inner.lock().await;

        tracing::debug!(?required_level, "update_current_level: updating current level");
        let res = inner.cpu.current_level.update(required_level).await;
        if let Err(error) = res {
            tracing::warn!(?error, "update_current_level: current_level.update failed");
            return Err(error.into());
        }

        // After other elements have been informed of required_level for cpu,
        // check whether the system can be suspended.
        if required_level == CpuLevel::Inactive.into_primitive() {
            tracing::debug!("beginning suspend process for cpu");
            inner.suspend_allowed = true;
            return Ok(true);
        } else {
            inner.suspend_allowed = false;
            return Ok(false);
        }
    }

    /// Gets a copy of the name of the CPU power element.
    pub async fn name(&self) -> String {
        self.inner.lock().await.cpu.name().to_string()
    }

    /// Gets a copy of the RequiredLevelProxy of the CPU power element.
    pub async fn required_level_proxy(&self) -> fbroker::RequiredLevelProxy {
        self.inner.lock().await.cpu.required_level.clone()
    }

    /// Attempts to suspend the system.
    ///
    /// Returns an enum representing the result of the suspend attempt.
    pub async fn trigger_suspend(&self) -> SuspendResult {
        let listener = self.suspend_resume_listener.get().unwrap();
        let mut suspend_failed = false;
        {
            tracing::debug!("trigger_suspend: acquiring inner lock");
            let inner = self.inner.lock().await;
            if !inner.suspend_allowed {
                tracing::info!("Suspend not allowed");
                return SuspendResult::NotAllowed;
            }

            self._inspect_node.borrow_mut().add_entry(|node| {
                node.record_int(
                    fobs::SUSPEND_ATTEMPTED_AT,
                    zx::MonotonicInstant::get().into_nanos(),
                );
            });
            // LINT.IfChange
            tracing::info!("Suspending");
            // LINT.ThenChange(//src/testing/end_to_end/honeydew/honeydew/affordances/starnix/system_power_state_controller.py)

            let response = if let Some(suspender) = inner.suspender.as_ref() {
                // LINT.IfChange
                fuchsia_trace::duration!(c"power", c"system-activity-governor:suspend");
                // LINT.ThenChange(//src/performance/lib/trace_processing/metrics/suspend.py)
                Some(
                    suspender
                        .suspend(&fhsuspend::SuspenderSuspendRequest {
                            state_index: Some(inner.suspend_state_index),
                            ..Default::default()
                        })
                        .await,
                )
            } else {
                None
            };
            // LINT.IfChange
            tracing::info!(?response, "Resuming");
            // LINT.ThenChange(//src/testing/end_to_end/honeydew/honeydew/affordances/starnix/system_power_state_controller.py)
            self._inspect_node.borrow_mut().add_entry(|node| {
                let time = zx::MonotonicInstant::get().into_nanos();
                if let Some(Ok(Ok(SuspendResponse { suspend_duration: Some(duration), .. }))) =
                    response
                {
                    node.record_int(fobs::SUSPEND_RESUMED_AT, time);
                    node.record_int(fobs::SUSPEND_LAST_TIMESTAMP, duration);
                } else {
                    node.record_int(fobs::SUSPEND_FAILED_AT, time);
                }
            });

            listener.suspend_stats().update(|stats_opt: &mut Option<fsuspend::SuspendStats>| {
                let stats = stats_opt.as_mut().expect("stats is uninitialized");

                match response {
                    Some(Ok(Ok(res))) => {
                        stats.last_time_in_suspend = res.suspend_duration;
                        stats.last_time_in_suspend_operations = res.suspend_overhead;

                        if stats.last_time_in_suspend.is_some() {
                            stats.success_count = stats.success_count.map(|c| c + 1);
                        } else {
                            tracing::warn!("Failed to suspend in Suspender");
                            suspend_failed = true;
                            stats.fail_count = stats.fail_count.map(|c| c + 1);
                        }
                    }
                    Some(error) => {
                        tracing::warn!(?error, "Failed to suspend");
                        stats.fail_count = stats.fail_count.map(|c| c + 1);
                        suspend_failed = true;

                        if let Ok(Err(error)) = error {
                            stats.last_failed_error = Some(error);
                        }
                    }
                    None => {
                        tracing::warn!("No suspender available, suspend was a no-op");
                        stats.fail_count = stats.fail_count.map(|c| c + 1);
                        stats.last_failed_error = Some(zx::sys::ZX_ERR_NOT_SUPPORTED);
                    }
                }
                true
            });
        }
        // At this point, the suspend request is no longer in flight and has been handled. With
        // `inner` going out of scope, other tasks can modify flags and update the power level of
        // CPU power element.
        listener.on_suspend_ended(!suspend_failed).await;
        if suspend_failed {
            SuspendResult::Fail
        } else {
            SuspendResult::Success
        }
    }
}

pub struct SuspendStatsManager {
    /// The hanging get handler used to notify subscribers of changes to suspend stats.
    hanging_get: RefCell<StatsHangingGet>,
    /// The publisher used to push changes to suspend stats.
    stats_publisher: StatsPublisher,
    /// The inspect node for suspend stats.
    inspect_node: INode,
    /// The inspect node that contains the number of successful suspend attempts.
    success_count_node: IUint,
    /// The inspect node that contains the number of failed suspend attempts.
    fail_count_node: IUint,
    /// The inspect node that contains the error code of the last failed suspend attempt.
    last_failed_error_node: IInt,
    /// The inspect node that contains the duration the platform spent in suspension in the last
    /// attempt.
    last_time_in_suspend_node: IInt,
    /// The inspect node that contains the duration the platform spent transitioning to a suspended
    /// state in the last attempt.
    last_time_in_suspend_operations_node: IInt,
}

impl SuspendStatsManager {
    fn new(inspect_node: INode) -> Self {
        let stats = fsuspend::SuspendStats {
            success_count: Some(0),
            fail_count: Some(0),
            ..Default::default()
        };

        let success_count_node = inspect_node
            .create_uint(fobs::SUSPEND_SUCCESS_COUNT, *stats.success_count.as_ref().unwrap_or(&0));
        let fail_count_node = inspect_node
            .create_uint(fobs::SUSPEND_FAIL_COUNT, *stats.fail_count.as_ref().unwrap_or(&0));
        let last_failed_error_node = inspect_node.create_int(
            fobs::SUSPEND_LAST_FAILED_ERROR,
            (*stats.last_failed_error.as_ref().unwrap_or(&0i32)).into(),
        );
        let last_time_in_suspend_node = inspect_node.create_int(
            fobs::SUSPEND_LAST_TIMESTAMP,
            *stats.last_time_in_suspend.as_ref().unwrap_or(&-1i64),
        );
        let last_time_in_suspend_operations_node = inspect_node.create_int(
            fobs::SUSPEND_LAST_DURATION,
            *stats.last_time_in_suspend_operations.as_ref().unwrap_or(&-1i64),
        );

        let hanging_get = StatsHangingGet::new(
            stats,
            Box::new(
                |stats: &fsuspend::SuspendStats, res: fsuspend::StatsWatchResponder| -> bool {
                    if let Err(error) = res.send(stats) {
                        tracing::warn!(?error, "Failed to send suspend stats to client");
                    }
                    true
                },
            ),
        );

        let stats_publisher = hanging_get.new_publisher();

        Self {
            hanging_get: RefCell::new(hanging_get),
            stats_publisher,
            inspect_node,
            success_count_node,
            fail_count_node,
            last_failed_error_node,
            last_time_in_suspend_node,
            last_time_in_suspend_operations_node,
        }
    }

    fn update<UpdateFn>(&self, update: UpdateFn)
    where
        UpdateFn: FnOnce(&mut Option<fsuspend::SuspendStats>) -> bool,
    {
        self.stats_publisher.update(|stats_opt| {
            let success = update(stats_opt);

            self.inspect_node.atomic_update(|_| {
                let stats = stats_opt.as_ref().expect("stats is uninitialized");
                self.success_count_node.set(*stats.success_count.as_ref().unwrap_or(&0));
                self.fail_count_node.set(*stats.fail_count.as_ref().unwrap_or(&0));
                self.last_failed_error_node
                    .set((*stats.last_failed_error.as_ref().unwrap_or(&0i32)).into());
                self.last_time_in_suspend_node
                    .set(*stats.last_time_in_suspend.as_ref().unwrap_or(&-1i64));
                self.last_time_in_suspend_operations_node
                    .set(*stats.last_time_in_suspend_operations.as_ref().unwrap_or(&-1i64));
            });

            tracing::info!(?success, ?stats_opt, "Updating suspend stats");
            success
        });
    }
}

/// Manager of leases that block execution state.
///
/// Used to facilitate the `TakeWakeLease()` and `TakeApplicationActivityLease()`
/// functionality of `fuchsia.power.system.ActivityGovernor`.
///
/// A wake lease blocks suspension by requiring the power level of the Execution
/// State to be at least [`ExecutionStateLevel::Suspending`].
///
/// An application activity lease requires Application Activity to be at least
/// [`ApplicationActivityLevel::Active`].
struct LeaseManager {
    /// The inspect node for lease stats.
    inspect_node: INode,
    /// Proxy to the power topology to create power elements.
    topology: fbroker::TopologyProxy,
    /// Dependency token for Execution State.
    execution_state_assertive_dependency_token: fbroker::DependencyToken,
    /// Dependency token for Application Activity.
    application_activity_assertive_dependency_token: fbroker::DependencyToken,
}

impl LeaseManager {
    pub fn new(
        inspect_node: INode,
        topology: fbroker::TopologyProxy,
        execution_state_assertive_dependency_token: fbroker::DependencyToken,
        application_activity_assertive_dependency_token: fbroker::DependencyToken,
    ) -> Self {
        Self {
            inspect_node,
            topology,
            execution_state_assertive_dependency_token,
            application_activity_assertive_dependency_token,
        }
    }

    async fn create_application_activity_lease(&self, name: String) -> Result<fsystem::LeaseToken> {
        let (server_token, client_token) = fsystem::LeaseToken::create();

        let lease_helper = LeaseHelper::new(
            &self.topology,
            &name,
            vec![power_broker_client::LeaseDependency {
                dependency_type: fbroker::DependencyType::Assertive,
                requires_token: self
                    .application_activity_assertive_dependency_token
                    .duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                requires_level_by_preference: vec![
                    ApplicationActivityLevel::Active.into_primitive()
                ],
            }],
        )
        .await?;
        tracing::debug!("Acquiring lease for '{}'", name);
        let lease = lease_helper.lease().await?;

        let token_info = server_token.basic_info()?;
        let inspect_lease_node =
            self.inspect_node.create_child(token_info.koid.raw_koid().to_string());
        let related_koid = token_info.related_koid.raw_koid();

        inspect_lease_node.record_string("name", name.clone());
        inspect_lease_node.record_string("type", "application_activity");
        inspect_lease_node.record_uint("client_token_koid", related_koid);
        NodeTimeExt::<zx::BootTimeline>::record_time(&inspect_lease_node, "created_at");

        fasync::Task::local(async move {
            // Keep lease alive for as long as the client keeps it alive.
            let _ = fasync::OnSignals::new(server_token, zx::Signals::EVENTPAIR_PEER_CLOSED).await;
            tracing::debug!("Dropping lease for '{}'", name);
            drop(inspect_lease_node);
            drop(lease);
        })
        .detach();

        Ok(client_token)
    }

    async fn create_wake_lease(&self, name: String) -> Result<fsystem::LeaseToken> {
        let (server_token, client_token) = fsystem::LeaseToken::create();

        let lease_helper =
            LeaseHelper::new(
                &self.topology,
                &name,
                vec![power_broker_client::LeaseDependency {
                    dependency_type: fbroker::DependencyType::Assertive,
                    requires_token: self
                        .execution_state_assertive_dependency_token
                        .duplicate_handle(zx::Rights::SAME_RIGHTS)?,
                    requires_level_by_preference: vec![
                        ExecutionStateLevel::Suspending.into_primitive()
                    ],
                }],
            )
            .await?;
        tracing::debug!("Acquiring lease for '{}'", name);
        let lease = lease_helper.lease().await?;

        let token_info = server_token.basic_info()?;
        let inspect_lease_node =
            self.inspect_node.create_child(token_info.koid.raw_koid().to_string());
        let related_koid = token_info.related_koid.raw_koid();

        inspect_lease_node.record_string("name", name.clone());
        inspect_lease_node.record_string("type", "wake");
        inspect_lease_node.record_uint("client_token_koid", related_koid);
        NodeTimeExt::<zx::BootTimeline>::record_time(&inspect_lease_node, "created_at");

        fasync::Task::local(async move {
            // Keep lease alive for as long as the client keeps it alive.
            let _ = fasync::OnSignals::new(server_token, zx::Signals::EVENTPAIR_PEER_CLOSED).await;
            tracing::debug!("Dropping lease for '{}'", name);
            drop(inspect_lease_node);
            drop(lease);
        })
        .detach();

        Ok(client_token)
    }
}

/// SystemActivityGovernor runs the server for fuchsia.power.suspend and fuchsia.power.system FIDL
/// APIs.
pub struct SystemActivityGovernor {
    /// The root inspect node for system-activity-governor.
    inspect_root: INode,
    /// The context used to manage the execution state power element.
    execution_state: PowerElementContext,
    /// The context used to manage the application activity power element.
    application_activity: PowerElementContext,
    /// The context used to manage the wake handling power element.
    wake_handling: PowerElementContext,
    /// Resume latency information, if available.
    resume_latency_ctx: Option<Rc<ResumeLatencyContext>>,
    /// The manager used to report suspend stats to inspect and clients of
    /// fuchsia.power.suspend.Stats.
    suspend_stats: SuspendStatsManager,
    /// The manager used to create and report wake and activity application leases.
    lease_manager: LeaseManager,
    /// The collection of ActivityGovernorListener that have registered through
    /// fuchsia.power.system.ActivityGovernor/RegisterListener.
    listeners: RefCell<Vec<fsystem::ActivityGovernorListenerProxy>>,
    /// The manager used to modify cpu power element and trigger suspend.
    cpu_manager: Rc<CpuManager>,
    /// The context used to manage the boot_control power element.
    boot_control: Rc<PowerElementContext>,
    /// The collection of information about PowerElements managed
    /// by system-activity-governor.
    element_power_level_names: Vec<fbroker::ElementPowerLevelNames>,
    /// The signal which is set when the power elements are configured and
    /// FIDL handlers can run. This is required because a newly constructed
    /// SystemActivityGovernor initializes and runs power elements
    /// asynchronously. This signal prevents exposing uninitialized power
    /// element state to external clients.
    is_running_signal: async_lock::OnceCell<()>,
    /// The flag which indicates a successful suspension.
    suspend_succeeded: Cell<bool>,
    /// The flag used to synchronize the resume_control_lease.
    /// It's set to true when a resume_control_lease is created and to false
    /// when it needs to be dropped.
    // TODO(https://fxbug.dev/372695129): Optimize resume_control_lease.
    waiting_for_es_activation_after_resume: Cell<bool>,
    /// The lease which hold execution_state at suspending state temporarily
    /// after suspension.
    resume_control_lease: RefCell<Option<fbroker::LeaseControlProxy>>,
}

impl SystemActivityGovernor {
    pub async fn new(
        topology: &fbroker::TopologyProxy,
        inspect_root: INode,
        cpu_manager: Rc<CpuManager>,
        execution_state_dependencies: Vec<fbroker::LevelDependency>,
    ) -> Result<Rc<Self>> {
        let mut element_power_level_names: Vec<fbroker::ElementPowerLevelNames> = Vec::new();

        element_power_level_names.push(generate_element_power_level_names(
            "cpu",
            vec![
                (CpuLevel::Inactive.into_primitive(), "Inactive".to_string()),
                (CpuLevel::Active.into_primitive(), "Active".to_string()),
            ],
        ));

        let execution_state = PowerElementContext::builder(
            topology,
            "execution_state",
            &[
                ExecutionStateLevel::Inactive.into_primitive(),
                ExecutionStateLevel::Suspending.into_primitive(),
                ExecutionStateLevel::Active.into_primitive(),
            ],
        )
        .dependencies(execution_state_dependencies)
        .build()
        .await
        .expect("PowerElementContext encountered error while building execution_state");

        element_power_level_names.push(generate_element_power_level_names(
            "execution_state",
            vec![
                (ExecutionStateLevel::Inactive.into_primitive(), "Inactive".to_string()),
                (ExecutionStateLevel::Suspending.into_primitive(), "Suspending".to_string()),
                (ExecutionStateLevel::Active.into_primitive(), "Active".to_string()),
            ],
        ));

        let application_activity = PowerElementContext::builder(
            topology,
            "application_activity",
            &[
                ApplicationActivityLevel::Inactive.into_primitive(),
                ApplicationActivityLevel::Active.into_primitive(),
            ],
        )
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Assertive,
            dependent_level: ApplicationActivityLevel::Active.into_primitive(),
            requires_token: execution_state
                .assertive_dependency_token()
                .expect("token not registered"),
            requires_level_by_preference: vec![ExecutionStateLevel::Active.into_primitive()],
        }])
        .build()
        .await
        .expect("PowerElementContext encountered error while building application_activity");

        let lease_manager = LeaseManager::new(
            inspect_root.create_child("wake_leases"),
            topology.clone(),
            execution_state.assertive_dependency_token().expect("token not registered"),
            application_activity.assertive_dependency_token().expect("token not registered"),
        );

        element_power_level_names.push(generate_element_power_level_names(
            "application_activity",
            vec![
                (ApplicationActivityLevel::Inactive.into_primitive(), "Inactive".to_string()),
                (ApplicationActivityLevel::Active.into_primitive(), "Active".to_string()),
            ],
        ));

        let wake_handling = PowerElementContext::builder(
            topology,
            "wake_handling",
            &[
                WakeHandlingLevel::Inactive.into_primitive(),
                WakeHandlingLevel::Active.into_primitive(),
            ],
        )
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Assertive,
            dependent_level: WakeHandlingLevel::Active.into_primitive(),
            requires_token: execution_state
                .assertive_dependency_token()
                .expect("token not registered"),
            requires_level_by_preference: vec![ExecutionStateLevel::Suspending.into_primitive()],
        }])
        .build()
        .await
        .expect("PowerElementContext encountered error while building wake_handling");

        element_power_level_names.push(generate_element_power_level_names(
            "wake_handling",
            vec![
                (WakeHandlingLevel::Inactive.into_primitive(), "Inactive".to_string()),
                (WakeHandlingLevel::Active.into_primitive(), "Active".to_string()),
            ],
        ));

        let boot_control = Rc::new(
            PowerElementContext::builder(
                topology,
                "boot_control",
                &[BootControlLevel::Inactive.into(), BootControlLevel::Active.into()],
            )
            .dependencies(vec![fbroker::LevelDependency {
                dependency_type: fbroker::DependencyType::Assertive,
                dependent_level: BootControlLevel::Active.into(),
                requires_token: execution_state
                    .assertive_dependency_token()
                    .expect("token not registered"),
                requires_level_by_preference: vec![ExecutionStateLevel::Active.into_primitive()],
            }])
            .build()
            .await
            .expect("PowerElementContext encountered error while building boot_control"),
        );
        let bc_context = boot_control.clone();
        fasync::Task::local(async move {
            run_power_element(
                &bc_context.name(),
                &bc_context.required_level,
                0,    /* initial_level */
                None, /* inspect_node */
                basic_update_fn_factory(&bc_context),
            )
            .await;
        })
        .detach();

        element_power_level_names.push(generate_element_power_level_names(
            "boot_control",
            vec![
                (BootControlLevel::Inactive.into(), "Inactive".to_string()),
                (BootControlLevel::Active.into(), "Active".to_string()),
            ],
        ));

        let resume_latency_ctx =
            if let Some(suspender) = &cpu_manager.inner.lock().await.suspender.clone() {
                let resume_latency_ctx = ResumeLatencyContext::new(suspender, topology).await?;
                element_power_level_names.push(generate_element_power_level_names(
                    "execution_resume_latency",
                    resume_latency_ctx
                        .resume_latencies
                        .iter()
                        .enumerate()
                        .map(|(i, val)| (i as u8, format!("{val} ns")))
                        .collect(),
                ));
                Some(Rc::new(resume_latency_ctx))
            } else {
                None
            };

        let suspend_stats =
            SuspendStatsManager::new(inspect_root.create_child(fobs::SUSPEND_STATS_NODE));

        Ok(Rc::new(Self {
            inspect_root,
            execution_state,
            application_activity,
            wake_handling,
            resume_latency_ctx,
            suspend_stats,
            lease_manager,
            listeners: RefCell::new(Vec::new()),
            cpu_manager,
            boot_control,
            element_power_level_names,
            suspend_succeeded: Cell::new(false),
            waiting_for_es_activation_after_resume: Cell::new(false),
            resume_control_lease: RefCell::new(None),
            is_running_signal: async_lock::OnceCell::new(),
        }))
    }

    /// Runs a FIDL server to handle fuchsia.power.suspend and fuchsia.power.system API requests.
    pub async fn run(self: &Rc<Self>, elements_node: &INode) -> Result<()> {
        tracing::info!("Handling power elements");

        self.run_execution_state(&elements_node);
        self.run_wake_handling(&elements_node);
        self.run_execution_resume_latency(&elements_node);

        tracing::info!("System is booting. Acquiring boot control lease.");
        let boot_control_lease = self
            .boot_control
            .lessor
            .lease(BootControlLevel::Active.into())
            .await
            .expect("Failed to request boot control lease")
            .expect("Failed to acquire boot control lease")
            .into_proxy()?;

        // TODO(https://fxbug.dev/333947976): Use RequiredLevel when LeaseStatus is removed.
        let mut lease_status = fbroker::LeaseStatus::Unknown;
        while lease_status != fbroker::LeaseStatus::Satisfied {
            lease_status = boot_control_lease.watch_status(lease_status).await.unwrap();
        }

        self.run_application_activity(
            &elements_node,
            &self.inspect_root,
            boot_control_lease.into_client_end().expect("failed to convert to ClientEnd"),
        );

        tracing::info!("Boot control required. Updating boot_control level to active.");
        let res = self.boot_control.current_level.update(BootControlLevel::Active.into()).await;
        if let Err(error) = res {
            tracing::warn!(?error, "failed to update boot_control level to Active");
        }

        let _ = self.is_running_signal.set(()).await;
        Ok(())
    }

    fn run_application_activity(
        self: &Rc<Self>,
        inspect_node: &INode,
        root_node: &INode,
        boot_control_lease: fidl::endpoints::ClientEnd<fbroker::LeaseControlMarker>,
    ) {
        let application_activity_node = inspect_node.create_child("application_activity");
        let booting_node = Rc::new(root_node.create_bool("booting", true));
        let this = self.clone();
        let this_clone = self.clone();

        fasync::Task::local(async move {
            let update_fn = Rc::new(basic_update_fn_factory(&this.application_activity));
            let boot_control_lease = Rc::new(RefCell::new(Some(boot_control_lease)));

            run_power_element(
                this.application_activity.name(),
                &this.application_activity.required_level,
                ApplicationActivityLevel::Inactive.into_primitive(),
                Some(application_activity_node),
                Box::new(move |new_power_level: fbroker::PowerLevel| {
                    let update_fn = update_fn.clone();
                    let boot_control_lease = boot_control_lease.clone();
                    let booting_node = booting_node.clone();
                    let this = this_clone.clone();

                    async move {
                        update_fn(new_power_level).await;

                        // TODO(https://fxbug.dev/333699275): When the boot indication API is
                        // available, this logic should be removed in favor of that.
                        if new_power_level != ApplicationActivityLevel::Inactive.into_primitive()
                            && boot_control_lease.borrow().is_some()
                        {
                            tracing::info!("System has booted. Dropping boot control lease.");
                            boot_control_lease.borrow_mut().take();
                            let res = this
                                .boot_control
                                .current_level
                                .update(BootControlLevel::Inactive.into())
                                .await;
                            if let Err(error) = res {
                                tracing::warn!(
                                    ?error,
                                    "update boot_control level to inactive failed"
                                );
                            }
                            booting_node.set(false);
                        }
                    }
                    .boxed_local()
                }),
            )
            .await;
        })
        .detach();
    }

    fn run_execution_state(self: &Rc<Self>, inspect_node: &INode) {
        let execution_state_node = inspect_node.create_child("execution_state");
        let this = self.clone();
        let this_clone = this.clone();

        fasync::Task::local(async move {
            let update_fn = Rc::new(basic_update_fn_factory(&this.execution_state));
            let previous_power_level =
                Rc::new(Cell::new(ExecutionStateLevel::Inactive.into_primitive()));

            run_power_element(
                &this.execution_state.name(),
                &this.execution_state.required_level,
                previous_power_level.get(),
                Some(execution_state_node),
                Box::new(move |new_power_level: fbroker::PowerLevel| {
                    let update_fn = update_fn.clone();
                    let previous_power_level = previous_power_level.clone();
                    let this = this_clone.clone();

                    async move {
                        // Call suspend callback before ExecutionState power level changes.
                        if new_power_level == ExecutionStateLevel::Inactive.into_primitive() {
                            this.notify_on_suspend().await;
                        } else if previous_power_level.get()
                            == ExecutionStateLevel::Inactive.into_primitive()
                        {
                            // If leaving Inactive, we need to notify listeners that we exited
                            // suspend. This cannot block, as a listener may need to raise Execution
                            // State.
                            let this2 = this.clone();
                            fasync::Task::local(async move {
                                this2.notify_suspend_ended().await;
                            })
                            .detach();
                        }

                        // If entering Active, SAG drops the resume control lease to re-enable
                        // suspension.
                        if new_power_level == ExecutionStateLevel::Active.into_primitive() {
                            this.waiting_for_es_activation_after_resume.set(false);
                            drop(this.resume_control_lease.borrow_mut().take());
                        }

                        update_fn(new_power_level).await;
                        previous_power_level.set(new_power_level);
                    }
                    .boxed_local()
                }),
            )
            .await;
        })
        .detach();
    }

    fn run_wake_handling(self: &Rc<Self>, inspect_node: &INode) {
        let wake_handling_node = inspect_node.create_child("wake_handling");
        let this = self.clone();

        fasync::Task::local(async move {
            run_power_element(
                this.wake_handling.name(),
                &this.wake_handling.required_level,
                WakeHandlingLevel::Inactive.into_primitive(),
                Some(wake_handling_node),
                basic_update_fn_factory(&this.wake_handling),
            )
            .await;
        })
        .detach();
    }

    fn run_execution_resume_latency(self: &Rc<Self>, inspect_node: &INode) {
        if let Some(resume_latency_ctx) = &self.resume_latency_ctx {
            let initial_level = 0;
            resume_latency_ctx.clone().run(
                self.cpu_manager.clone(),
                initial_level,
                inspect_node.create_child("execution_resume_latency"),
            );
        }
    }

    async fn get_status_endpoints(&self) -> Vec<fbroker::ElementStatusEndpoint> {
        let mut endpoints = Vec::new();

        register_element_status_endpoint("execution_state", &self.execution_state, &mut endpoints);

        register_element_status_endpoint(
            "application_activity",
            &self.application_activity,
            &mut endpoints,
        );

        register_element_status_endpoint(
            "cpu",
            &self.cpu_manager.inner.lock().await.cpu,
            &mut endpoints,
        );

        register_element_status_endpoint("wake_handling", &self.wake_handling, &mut endpoints);

        register_element_status_endpoint("boot_control", &self.boot_control, &mut endpoints);

        if let Some(resume_latency_ctx) = &self.resume_latency_ctx {
            register_element_status_endpoint(
                "execution_resume_latency",
                &resume_latency_ctx.execution_resume_latency,
                &mut endpoints,
            );
        }

        endpoints
    }

    pub async fn handle_activity_governor_stream(
        self: Rc<Self>,
        mut stream: fsystem::ActivityGovernorRequestStream,
    ) {
        // Before handling requests, ensure power elements are initialized and handlers are running.
        self.is_running_signal.wait().await;
        while let Some(request) = stream.next().await {
            match request {
                Ok(fsystem::ActivityGovernorRequest::GetPowerElements { responder }) => {
                    let result = responder.send(fsystem::PowerElements {
                        execution_state: Some(fsystem::ExecutionState {
                            opportunistic_dependency_token: Some(
                                self.execution_state
                                    .opportunistic_dependency_token()
                                    .expect("token not registered"),
                            ),
                            ..Default::default()
                        }),
                        application_activity: Some(fsystem::ApplicationActivity {
                            assertive_dependency_token: Some(
                                self.application_activity
                                    .assertive_dependency_token()
                                    .expect("token not registered"),
                            ),
                            ..Default::default()
                        }),
                        wake_handling: Some(fsystem::WakeHandling {
                            assertive_dependency_token: Some(
                                self.wake_handling
                                    .assertive_dependency_token()
                                    .expect("token not registered"),
                            ),
                            ..Default::default()
                        }),
                        execution_resume_latency: self
                            .resume_latency_ctx
                            .as_ref()
                            .map(|r| r.to_fidl()),
                        ..Default::default()
                    });

                    if let Err(error) = result {
                        tracing::warn!(
                            ?error,
                            "Encountered error while responding to GetPowerElements request"
                        );
                    }
                }
                Ok(fsystem::ActivityGovernorRequest::TakeApplicationActivityLease {
                    responder,
                    name,
                }) => {
                    let client_token =
                        match self.lease_manager.create_application_activity_lease(name).await {
                            Ok(client_token) => client_token,
                            Err(error) => {
                                tracing::warn!(
                                ?error,
                                "Encountered error while registering application activity lease"
                            );
                                return;
                            }
                        };

                    if let Err(error) = responder.send(client_token) {
                        tracing::warn!(
                            ?error,
                            "Encountered error while responding to TakeApplicationActivity request"
                        );
                    }
                }
                Ok(fsystem::ActivityGovernorRequest::TakeWakeLease { responder, name }) => {
                    let client_token = match self.lease_manager.create_wake_lease(name).await {
                        Ok(client_token) => client_token,
                        Err(error) => {
                            tracing::warn!(
                                ?error,
                                "Encountered error while registering wake lease"
                            );
                            return;
                        }
                    };

                    if let Err(error) = responder.send(client_token) {
                        tracing::warn!(
                            ?error,
                            "Encountered error while responding to TakeWakeLease request"
                        );
                    }
                }
                Ok(fsystem::ActivityGovernorRequest::RegisterListener { responder, payload }) => {
                    match payload.listener {
                        Some(listener) => {
                            self.listeners.borrow_mut().push(listener.into_proxy().unwrap());
                        }
                        None => tracing::warn!("No listener provided in request"),
                    }
                    let _ = responder.send();
                }
                Ok(fsystem::ActivityGovernorRequest::_UnknownMethod { ordinal, .. }) => {
                    tracing::warn!(?ordinal, "Unknown ActivityGovernorRequest method");
                }
                Err(error) => {
                    tracing::error!(?error, "Error handling ActivityGovernor request stream");
                }
            }
        }
    }

    pub async fn handle_stats_stream(self: Rc<Self>, mut stream: fsuspend::StatsRequestStream) {
        // Before handling requests, ensure power elements are initialized and handlers are running.
        self.is_running_signal.wait().await;
        let sub = self.suspend_stats.hanging_get.borrow_mut().new_subscriber();

        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsuspend::StatsRequest::Watch { responder } => {
                    if let Err(error) = sub.register(responder) {
                        tracing::warn!(?error, "Failed to register for Watch call");
                    }
                }
                fsuspend::StatsRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!(?ordinal, "Unknown StatsRequest method");
                }
            }
        }
    }

    pub async fn handle_element_info_provider_stream(
        self: Rc<Self>,
        mut stream: fbroker::ElementInfoProviderRequestStream,
    ) {
        // Before handling requests, ensure power elements are initialized and handlers are running.
        self.is_running_signal.wait().await;
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fbroker::ElementInfoProviderRequest::GetElementPowerLevelNames { responder } => {
                    let result = responder.send(Ok(&self.element_power_level_names));
                    if let Err(error) = result {
                        tracing::warn!(
                            ?error,
                            "Encountered error while responding to GetElementPowerLevelNames request"
                        );
                    }
                }
                fbroker::ElementInfoProviderRequest::GetStatusEndpoints { responder } => {
                    let result = responder.send(Ok(self.get_status_endpoints().await));
                    if let Err(error) = result {
                        tracing::warn!(
                            ?error,
                            "Encountered error while responding to GetStatusEndpoints request"
                        );
                    }
                }
                fbroker::ElementInfoProviderRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!(?ordinal, "Unknown ElementInfoProviderRequest method");
                }
            }
        }
    }
}

#[async_trait(?Send)]
impl SuspendResumeListener for SystemActivityGovernor {
    fn suspend_stats(&self) -> &SuspendStatsManager {
        &self.suspend_stats
    }

    async fn on_suspend_ended(&self, suspend_succeeded: bool) {
        tracing::debug!(?suspend_succeeded, "on_suspend_ended");
        self.suspend_succeeded.set(suspend_succeeded);
        self.waiting_for_es_activation_after_resume.set(true);

        let lease = self
            .execution_state
            .lessor
            .lease(ExecutionStateLevel::Suspending.into_primitive())
            .await
            .expect("Failed to request ExecutionState lease")
            .expect("Failed to acquire ExecutionState lease")
            .into_proxy()
            .expect("Failed to convert the ExecutionState lease ClientEnd into a Proxy");

        // TODO(https://fxbug.dev/333947976): Use RequiredLevel when LeaseStatus is removed.
        let mut lease_status = fbroker::LeaseStatus::Unknown;
        while lease_status != fbroker::LeaseStatus::Satisfied {
            lease_status = lease.watch_status(lease_status).await.unwrap();
        }

        if self.waiting_for_es_activation_after_resume.get() {
            let _ = self.resume_control_lease.borrow_mut().insert(lease);
        }
    }

    async fn notify_on_suspend(&self) {
        tracing::debug!("notify_on_suspend");
        // A client may call RegisterListener while handling on_suspend which may cause another
        // mutable borrow of listeners. Clone the listeners to prevent this.
        let listeners: Vec<_> = self.listeners.borrow_mut().clone();
        for l in listeners {
            // TODO(b/363055581): Remove the timeout when it is no longer needed.
            let _ = l.on_suspend_started().await;
        }
    }

    async fn notify_suspend_ended(&self) {
        let suspend_succeeded = self.suspend_succeeded.get();
        tracing::debug!(?suspend_succeeded, "notify_suspend_ended");
        if suspend_succeeded {
            self.notify_on_resume().await;
            self.suspend_succeeded.set(false);
        } else {
            self.notify_on_suspend_fail().await;
        }
    }

    async fn notify_on_resume(&self) {
        // A client may call RegisterListener while handling on_resume which may cause another
        // mutable borrow of listeners. Clone the listeners to prevent this.
        let listeners: Vec<_> = self.listeners.borrow_mut().clone();
        for l in listeners {
            let _ = l.on_resume().await;
        }
    }

    async fn notify_on_suspend_fail(&self) {
        // A client may call RegisterListener while handling on_suspend_fail which may cause another
        // mutable borrow of listeners. Clone the listeners to prevent this.
        let listeners: Vec<_> = self.listeners.borrow_mut().clone();
        for l in listeners {
            let _ = l.on_suspend_fail().await;
        }
    }
}

struct ResumeLatencyContext {
    /// The context used to manage the execution resume latency power element.
    execution_resume_latency: PowerElementContext,
    /// The collection of resume latencies supported by the suspender.
    resume_latencies: Vec<zx::sys::zx_duration_t>,
}

impl ResumeLatencyContext {
    async fn new(
        suspender: &fhsuspend::SuspenderProxy,
        topology: &fbroker::TopologyProxy,
    ) -> Result<Self> {
        let resp = suspender
            .get_suspend_states()
            .await
            .expect("FIDL error encountered while calling Suspender")
            .expect("Suspender returned error when getting suspend states");
        let suspend_states =
            resp.suspend_states.expect("Suspend HAL did not return any suspend states");
        tracing::info!(?suspend_states, "Got suspend states from suspend HAL");

        let resume_latencies: Vec<_> = suspend_states
            .iter()
            .map(|s| s.resume_latency.expect("resume_latency not given"))
            .collect();
        let latency_count = resume_latencies.len().try_into()?;

        let execution_resume_latency = PowerElementContext::builder(
            topology,
            "execution_resume_latency",
            &Vec::from_iter(0..latency_count),
        )
        .build()
        .await
        .expect("PowerElementContext encountered error while building execution_resume_latency");

        Ok(Self { resume_latencies, execution_resume_latency })
    }

    fn to_fidl(&self) -> fsystem::ExecutionResumeLatency {
        fsystem::ExecutionResumeLatency {
            opportunistic_dependency_token: Some(
                self.execution_resume_latency
                    .opportunistic_dependency_token()
                    .expect("token not registered"),
            ),
            assertive_dependency_token: Some(
                self.execution_resume_latency
                    .assertive_dependency_token()
                    .expect("token not registered"),
            ),
            resume_latencies: Some(self.resume_latencies.clone()),
            ..Default::default()
        }
    }

    fn run(
        self: Rc<Self>,
        cpu_manager: Rc<CpuManager>,
        initial_level: u8,
        execution_resume_latency_node: INode,
    ) {
        let resume_latencies_node = execution_resume_latency_node
            .create_int_array("resume_latencies", self.resume_latencies.len());
        for (i, val) in self.resume_latencies.iter().enumerate() {
            resume_latencies_node.set(i, *val);
        }

        execution_resume_latency_node.record(resume_latencies_node);
        let resume_latency_node = Rc::new(
            execution_resume_latency_node.create_int("resume_latency", self.resume_latencies[0]),
        );

        let this = self.clone();
        fasync::Task::local(async move {
            let update_fn = Rc::new(basic_update_fn_factory(&this.execution_resume_latency));

            let resume_latency_ctx = this.clone();
            run_power_element(
                this.execution_resume_latency.name(),
                &this.execution_resume_latency.required_level,
                initial_level,
                Some(execution_resume_latency_node),
                Box::new(move |new_power_level: fbroker::PowerLevel| {
                    let this = resume_latency_ctx.clone();
                    let cpu_manager = cpu_manager.clone();
                    let update_fn = update_fn.clone();
                    let resume_latency_node = resume_latency_node.clone();

                    async move {
                        // new_power_level for execution_resume_latency is an index into
                        // the list of resume latencies returned by the suspend HAL.

                        // Before other power elements are informed of the new power level,
                        // update the value that will be sent to the suspend HAL when suspension
                        // is triggered to avoid data races.
                        if (new_power_level as usize) < this.resume_latencies.len() {
                            cpu_manager.set_suspend_state_index(new_power_level.into()).await;
                        }

                        update_fn(new_power_level).await;

                        // After other power elements are informed of the new power level,
                        // update Inspect to account for the new resume latency value.
                        let power_level = new_power_level as usize;
                        if power_level < this.resume_latencies.len() {
                            resume_latency_node.set(this.resume_latencies[power_level]);
                        }
                    }
                    .boxed_local()
                }),
            )
            .await;
        })
        .detach();
    }
}

fn register_element_status_endpoint(
    name: &str,
    element: &PowerElementContext,
    endpoints: &mut Vec<fbroker::ElementStatusEndpoint>,
) {
    let (status_client, status_server) = create_endpoints::<fbroker::StatusMarker>();
    match element.element_control.open_status_channel(status_server) {
        Ok(_) => {
            endpoints.push(fbroker::ElementStatusEndpoint {
                identifier: Some(name.into()),
                status: Some(status_client),
                ..Default::default()
            });
        }
        Err(error) => {
            tracing::warn!(?error, "Failed to register a Status channel for {}", name)
        }
    }
}

fn generate_element_power_level_names(
    element_name: &str,
    power_levels_names: Vec<(fbroker::PowerLevel, String)>,
) -> fbroker::ElementPowerLevelNames {
    fbroker::ElementPowerLevelNames {
        identifier: Some(element_name.into()),
        levels: Some(
            power_levels_names
                .iter()
                .cloned()
                .map(|(level, name)| fbroker::PowerLevelName {
                    level: Some(level),
                    name: Some(name.into()),
                    ..Default::default()
                })
                .collect(),
        ),
        ..Default::default()
    }
}
