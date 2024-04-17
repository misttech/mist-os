// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use async_utils::hanging_get::server::{HangingGet, Publisher};
use fidl_fuchsia_hardware_suspend as fhsuspend;
use fidl_fuchsia_power_broker as fbroker;
use fidl_fuchsia_power_suspend as fsuspend;
use fidl_fuchsia_power_system::{
    self as fsystem, ApplicationActivityLevel, ExecutionStateLevel, FullWakeHandlingLevel,
    WakeHandlingLevel,
};
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::{ArrayProperty, Property};
use fuchsia_zircon::{self as zx, HandleBased};
use futures::{
    channel::mpsc::{self, Receiver, Sender},
    future::LocalBoxFuture,
    lock::Mutex,
    prelude::*,
};
use power_broker_client::PowerElementContext;
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

type NotifyFn = Box<dyn Fn(&fsuspend::SuspendStats, fsuspend::StatsWatchResponder) -> bool>;
type StatsHangingGet = HangingGet<fsuspend::SuspendStats, fsuspend::StatsWatchResponder, NotifyFn>;
type StatsPublisher = Publisher<fsuspend::SuspendStats, fsuspend::StatsWatchResponder, NotifyFn>;

enum IncomingRequest {
    ActivityGovernor(fsystem::ActivityGovernorRequestStream),
    Stats(fsuspend::StatsRequestStream),
}

#[derive(Copy, Clone)]
enum BootControlLevel {
    Inactive,
    Active,
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
trait SuspendResumeListener {
    /// Gets the manager of suspend stats.
    fn suspend_stats(&self) -> &SuspendStatsManager;
    /// Called when system suspension is about to begin.
    fn on_suspend(&self);
    /// Called after system suspension ends.
    async fn on_resume(&self);
}

/// Controls access to execution_state and suspend management.
struct ExecutionStateManagerInner {
    /// The context used to manage the execution state power element.
    execution_state: PowerElementContext,
    /// The FIDL proxy to the device used to trigger system suspend.
    suspender: fhsuspend::SuspenderProxy,
    /// The suspend state index that will be passed to the suspender when system suspend is
    /// triggered.
    suspend_state_index: u64,
    /// The flag used to track whether suspension is allowed based on execution_state's power level.
    /// If true, execution_state has transitioned from a higher power state to
    /// ExecutionStateLevel::Inactive and is still at the ExecutionStateLevel::Inactive power level.
    suspend_allowed: bool,
}

/// Manager of the execution_state power element and suspend logic.
struct ExecutionStateManager {
    /// The passive dependency token of the execution_state power element.
    passive_dependency_token: fbroker::DependencyToken,
    /// State of the execution_state power element and suspend controls.
    inner: Mutex<ExecutionStateManagerInner>,
    /// SuspendResumeListener object to notify of suspend/resume.
    suspend_resume_listener: OnceCell<Rc<dyn SuspendResumeListener>>,
}

impl ExecutionStateManager {
    /// Creates a new ExecutionStateManager.
    fn new(execution_state: PowerElementContext, suspender: fhsuspend::SuspenderProxy) -> Self {
        Self {
            passive_dependency_token: execution_state.passive_dependency_token(),
            inner: Mutex::new(ExecutionStateManagerInner {
                execution_state,
                suspender,
                suspend_state_index: 0,
                suspend_allowed: false,
            }),
            suspend_resume_listener: OnceCell::new(),
        }
    }

    /// Sets the suspend resume listener.
    /// The listener can only be set once. Subsequent calls will result in a panic.
    fn set_suspend_resume_listener(&self, suspend_resume_listener: Rc<dyn SuspendResumeListener>) {
        self.suspend_resume_listener
            .set(suspend_resume_listener)
            .map_err(|_| anyhow::anyhow!("suspend_resume_listener is already set"))
            .unwrap();
    }

    /// Gets a copy of the passive dependency token.
    fn passive_dependency_token(&self) -> fbroker::DependencyToken {
        self.passive_dependency_token
            .duplicate_handle(zx::Rights::SAME_RIGHTS)
            .expect("failed to duplicate token")
    }

    /// Sets the suspend state index that will be used when suspend is triggered.
    async fn set_suspend_state_index(&self, suspend_state_index: u64) {
        tracing::debug!(?suspend_state_index, "set_suspend_state_index: acquiring inner lock");
        self.inner.lock().await.suspend_state_index = suspend_state_index;
    }

    /// Updates the power level of the execution state power element.
    ///
    /// Returns a Result that indicates whether the system should suspend or not.
    /// If an error occurs while updating the power level, the error is forwarded to the caller.
    async fn update_current_level(&self, required_level: fbroker::PowerLevel) -> Result<bool> {
        tracing::debug!(?required_level, "update_current_level: acquiring inner lock");
        let mut inner = self.inner.lock().await;

        tracing::debug!(?required_level, "update_current_level: updating current level");
        let res = inner.execution_state.current_level.update(required_level).await;
        if let Err(error) = res {
            tracing::warn!(?error, "update_current_level: current_level.update failed");
            return Err(error.into());
        }

        // After other elements have been informed of required_level for
        // execution_state, check whether the system can be suspended.
        if required_level == ExecutionStateLevel::Inactive.into_primitive() {
            tracing::debug!("beginning suspend process for execution_state");
            inner.suspend_allowed = true;
            return Ok(true);
        } else {
            inner.suspend_allowed = false;
            return Ok(false);
        }
    }

    /// Gets a copy of the name of the execution state power element.
    async fn name(&self) -> String {
        self.inner.lock().await.execution_state.name().to_string()
    }

    /// Gets a copy of the RequiredLevelProxy of the execution state power element.
    async fn required_level_proxy(&self) -> fbroker::RequiredLevelProxy {
        self.inner.lock().await.execution_state.required_level.clone()
    }

    /// Attempts to suspend the system.
    async fn trigger_suspend(&self) {
        let listener = self.suspend_resume_listener.get().unwrap();
        {
            tracing::debug!("trigger_suspend: acquiring inner lock");
            let inner = self.inner.lock().await;
            if !inner.suspend_allowed {
                tracing::info!("Suspend not allowed");
                return;
            }

            tracing::info!("Suspending");
            listener.on_suspend();

            let response = inner
                .suspender
                .suspend(&fhsuspend::SuspenderSuspendRequest {
                    state_index: Some(inner.suspend_state_index),
                    ..Default::default()
                })
                .await;
            tracing::info!(?response, "Resuming");

            listener.suspend_stats().update(|stats_opt: &mut Option<fsuspend::SuspendStats>| {
                let stats = stats_opt.as_mut().expect("stats is uninitialized");

                match response {
                    Ok(Ok(res)) => {
                        stats.last_time_in_suspend = res.suspend_duration;
                        stats.last_time_in_suspend_operations = res.suspend_overhead;

                        if stats.last_time_in_suspend.is_some() {
                            stats.success_count = stats.success_count.map(|c| c + 1);
                        } else {
                            tracing::warn!("Failed to suspend in Suspender");
                            stats.fail_count = stats.fail_count.map(|c| c + 1);
                        }
                    }
                    error => {
                        tracing::warn!(?error, "Failed to suspend");
                        stats.fail_count = stats.fail_count.map(|c| c + 1);

                        if let Ok(Err(error)) = error {
                            stats.last_failed_error = Some(error);
                        }
                    }
                }
                true
            });
        }
        // At this point, the suspend request is no longer in flight and has
        // been handled. With `inner` going out of scope, other tasks can modify
        // flags and update the power level of execution_state. This is needed
        // in order to allow listeners to request power level changes when they
        // get the `ActivityGovernorListener::OnResume` call.

        listener.on_resume().await;
    }
}

struct SuspendStatsManager {
    /// The hanging get handler used to notify subscribers of changes to suspend stats.
    hanging_get: RefCell<StatsHangingGet>,
    /// The publisher used to push changes to suspend stats.
    stats_publisher: StatsPublisher,
    /// The inspect node for suspend stats.
    inspect_node: fuchsia_inspect::Node,
    /// The inspect node that contains the number of successful suspend attempts.
    success_count_node: fuchsia_inspect::UintProperty,
    /// The inspect node that contains the number of failed suspend attempts.
    fail_count_node: fuchsia_inspect::UintProperty,
    /// The inspect node that contains the error code of the last failed suspend attempt.
    last_failed_error_node: fuchsia_inspect::IntProperty,
    /// The inspect node that contains the duration the platform spent in suspension in the last
    /// attempt.
    last_time_in_suspend_node: fuchsia_inspect::IntProperty,
    /// The inspect node that contains the duration the platform spent transitioning to a suspended
    /// state in the last attempt.
    last_time_in_suspend_operations_node: fuchsia_inspect::IntProperty,
}

impl SuspendStatsManager {
    fn new(inspect_node: fuchsia_inspect::Node) -> Self {
        let stats = fsuspend::SuspendStats {
            success_count: Some(0),
            fail_count: Some(0),
            ..Default::default()
        };

        let success_count_node =
            inspect_node.create_uint("success_count", *stats.success_count.as_ref().unwrap_or(&0));
        let fail_count_node =
            inspect_node.create_uint("fail_count", *stats.fail_count.as_ref().unwrap_or(&0));
        let last_failed_error_node = inspect_node.create_int(
            "last_failed_error",
            (*stats.last_failed_error.as_ref().unwrap_or(&0i32)).into(),
        );
        let last_time_in_suspend_node = inspect_node.create_int(
            "last_time_in_suspend",
            *stats.last_time_in_suspend.as_ref().unwrap_or(&-1i64),
        );
        let last_time_in_suspend_operations_node = inspect_node.create_int(
            "last_time_in_suspend_operations",
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

fn default_update_fn<'a>(
    power_element: &'a PowerElementContext,
) -> Box<dyn Fn(fbroker::PowerLevel) -> LocalBoxFuture<'a, ()> + 'a> {
    Box::new(move |new_power_level: fbroker::PowerLevel| {
        async move {
            let element_name = power_element.name();

            tracing::debug!(
                ?element_name,
                ?new_power_level,
                "default_update_fn: updating current level"
            );

            let res = power_element.current_level.update(new_power_level).await;
            if let Err(error) = res {
                tracing::warn!(
                    ?element_name,
                    ?error,
                    "default_update_fn: updating current level failed"
                );
            }
        }
        .boxed_local()
    })
}

async fn run_power_element<'a>(
    element_name: &'a str,
    required_level: &'a fbroker::RequiredLevelProxy,
    initial_level: fbroker::PowerLevel,
    inspect_node: fuchsia_inspect::Node,
    update_fn: Box<dyn Fn(fbroker::PowerLevel) -> LocalBoxFuture<'a, ()> + 'a>,
) {
    let mut last_required_level = initial_level;
    let power_level_node = inspect_node.create_uint("power_level", last_required_level.into());

    loop {
        tracing::debug!(
            ?element_name,
            ?last_required_level,
            "run_power_element: waiting for new level"
        );
        match required_level.watch().await {
            Ok(Ok(required_level)) => {
                tracing::debug!(
                    ?element_name,
                    ?required_level,
                    ?last_required_level,
                    "run_power_element: new level requested"
                );
                if required_level == last_required_level {
                    tracing::debug!(
                        ?element_name,
                        ?required_level,
                        ?last_required_level,
                        "run_power_element: required level has not changed, skipping."
                    );
                    continue;
                }

                update_fn(required_level).await;
                power_level_node.set(required_level.into());
                last_required_level = required_level;
            }
            error => {
                tracing::warn!(
                    ?element_name,
                    ?error,
                    "run_power_element: watch_required_level failed"
                )
            }
        }
    }
}

/// SystemActivityGovernor runs the server for fuchsia.power.suspend and fuchsia.power.system FIDL
/// APIs.
pub struct SystemActivityGovernor {
    /// The root inspect node for system-activity-governor.
    inspect_root: fuchsia_inspect::Node,
    /// The context used to manage the application activity power element.
    application_activity: PowerElementContext,
    /// The context used to manage the full wake handling power element.
    full_wake_handling: PowerElementContext,
    /// The context used to manage the wake handling power element.
    wake_handling: PowerElementContext,
    /// The context used to manage the execution resume latency power element.
    execution_resume_latency: PowerElementContext,
    /// The manager used to report suspend stats to inspect and clients of
    /// fuchsia.power.suspend.Stats.
    suspend_stats: SuspendStatsManager,
    /// The collection of resume latencies supported by the suspender.
    resume_latencies: Vec<zx::sys::zx_duration_t>,
    /// The collection of ActivityGovernorListener that have registered through
    /// fuchsia.power.system.ActivityGovernor/RegisterListener.
    listeners: RefCell<Vec<fsystem::ActivityGovernorListenerProxy>>,
    /// The manager used to modify execution_state and trigger suspend.
    execution_state_manager: ExecutionStateManager,
    /// The context used to manage the boot_control power element.
    boot_control: PowerElementContext,
}

impl SystemActivityGovernor {
    pub async fn new(
        topology: &fbroker::TopologyProxy,
        inspect_root: fuchsia_inspect::Node,
        suspender: fhsuspend::SuspenderProxy,
    ) -> Result<Rc<Self>> {
        let execution_state = PowerElementContext::builder(
            topology,
            "execution_state",
            &[
                ExecutionStateLevel::Inactive.into_primitive(),
                ExecutionStateLevel::WakeHandling.into_primitive(),
                ExecutionStateLevel::Active.into_primitive(),
            ],
        )
        .build()
        .await
        .expect("PowerElementContext encountered error while building execution_state");

        let application_activity = PowerElementContext::builder(
            topology,
            "application_activity",
            &[
                ApplicationActivityLevel::Inactive.into_primitive(),
                ApplicationActivityLevel::Active.into_primitive(),
            ],
        )
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Active,
            dependent_level: ApplicationActivityLevel::Active.into_primitive(),
            requires_token: execution_state.active_dependency_token(),
            requires_level: ExecutionStateLevel::Active.into_primitive(),
        }])
        .build()
        .await
        .expect("PowerElementContext encountered error while building application_activity");

        let full_wake_handling = PowerElementContext::builder(
            topology,
            "full_wake_handling",
            &[
                FullWakeHandlingLevel::Inactive.into_primitive(),
                FullWakeHandlingLevel::Active.into_primitive(),
            ],
        )
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Active,
            dependent_level: FullWakeHandlingLevel::Active.into_primitive(),
            requires_token: execution_state.active_dependency_token(),
            requires_level: ExecutionStateLevel::WakeHandling.into_primitive(),
        }])
        .build()
        .await
        .expect("PowerElementContext encountered error while building full_wake_handling");

        let wake_handling = PowerElementContext::builder(
            topology,
            "wake_handling",
            &[
                WakeHandlingLevel::Inactive.into_primitive(),
                WakeHandlingLevel::Active.into_primitive(),
            ],
        )
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Active,
            dependent_level: WakeHandlingLevel::Active.into_primitive(),
            requires_token: execution_state.active_dependency_token(),
            requires_level: ExecutionStateLevel::WakeHandling.into_primitive(),
        }])
        .build()
        .await
        .expect("PowerElementContext encountered error while building wake_handling");

        let boot_control = PowerElementContext::builder(
            topology,
            "boot_control",
            &[BootControlLevel::Inactive.into(), BootControlLevel::Active.into()],
        )
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Active,
            dependent_level: BootControlLevel::Active.into(),
            requires_token: execution_state.active_dependency_token(),
            requires_level: ExecutionStateLevel::Active.into_primitive(),
        }])
        .build()
        .await
        .expect("PowerElementContext encountered error while building wake_handling");

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

        let suspend_stats = SuspendStatsManager::new(inspect_root.create_child("suspend_stats"));

        Ok(Rc::new(Self {
            inspect_root,
            application_activity,
            full_wake_handling,
            wake_handling,
            execution_resume_latency,
            resume_latencies,
            suspend_stats,
            listeners: RefCell::new(Vec::new()),
            execution_state_manager: ExecutionStateManager::new(execution_state, suspender),
            boot_control,
        }))
    }

    /// Runs a FIDL server to handle fuchsia.power.suspend and fuchsia.power.system API requests.
    pub async fn run(self: Rc<Self>) -> Result<()> {
        tracing::info!("Handling power elements");
        self.execution_state_manager.set_suspend_resume_listener(self.clone());
        self.inspect_root.atomic_update(|node| {
            node.record_child("power_elements", |elements_node| {
                let (es_suspend_tx, es_suspend_rx) = mpsc::channel(1);
                self.run_suspend_task(es_suspend_rx);
                self.run_execution_state(&elements_node, es_suspend_tx);
                self.run_application_activity(&elements_node, &node);
                self.run_full_wake_handling(&elements_node);
                self.run_wake_handling(&elements_node);
                self.run_execution_resume_latency(elements_node);
            });
        });

        tracing::info!("Starting FIDL server");
        self.run_fidl_server().await
    }

    fn run_suspend_task(self: &Rc<Self>, mut execution_state_suspend_signal: Receiver<()>) {
        let this = self.clone();

        fasync::Task::local(async move {
            loop {
                tracing::debug!("awaiting suspend signals");
                let _ = execution_state_suspend_signal.next().await;

                // Check that the conditions to suspend are still satisfied.
                tracing::debug!("attempting to suspend");
                this.execution_state_manager.trigger_suspend().await;
            }
        })
        .detach();
    }

    fn run_execution_state(
        self: &Rc<Self>,
        inspect_node: &fuchsia_inspect::Node,
        execution_state_suspend_signaller: Sender<()>,
    ) {
        let execution_state_node = inspect_node.create_child("execution_state");
        let sag = self.clone();

        fasync::Task::local(async move {
            let sag = sag.clone();
            let element_name = sag.execution_state_manager.name().await;
            let required_level = sag.execution_state_manager.required_level_proxy().await;

            run_power_element(
                &element_name,
                &required_level,
                ExecutionStateLevel::Inactive.into_primitive(),
                execution_state_node,
                Box::new(move |new_power_level: fbroker::PowerLevel| {
                    let sag = sag.clone();
                    let mut execution_state_suspend_signaller =
                        execution_state_suspend_signaller.clone();

                    async move {
                        let update_res =
                            sag.execution_state_manager.update_current_level(new_power_level).await;
                        if let Ok(true) = update_res {
                            let _ = execution_state_suspend_signaller.start_send(());
                        }
                    }
                    .boxed_local()
                }),
            )
            .await;
        })
        .detach();
    }

    fn run_application_activity(
        self: &Rc<Self>,
        inspect_node: &fuchsia_inspect::Node,
        root_node: &fuchsia_inspect::Node,
    ) {
        let application_activity_node = inspect_node.create_child("application_activity");
        let booting_node = Rc::new(root_node.create_bool("booting", false));
        let this = self.clone();

        fasync::Task::local(async move {
            let update_fn = Rc::new(default_update_fn(&this.application_activity));

            tracing::info!("System is booting. Acquiring boot control lease.");
            let boot_control_lease = this
                .boot_control
                .lessor
                .lease(BootControlLevel::Active.into())
                .await
                .expect("Failed to acquire boot control lease");
            booting_node.set(true);
            let boot_control_lease = Rc::new(RefCell::new(Some(boot_control_lease)));

            run_power_element(
                this.application_activity.name(),
                &this.application_activity.required_level,
                ApplicationActivityLevel::Inactive.into_primitive(),
                application_activity_node,
                Box::new(move |new_power_level: fbroker::PowerLevel| {
                    let update_fn = update_fn.clone();
                    let boot_control_lease = boot_control_lease.clone();
                    let booting_node = booting_node.clone();

                    async move {
                        update_fn(new_power_level).await;

                        // TODO(https://fxbug.dev/333699275): When the boot indication API is
                        // available, this logic should be removed in favor of that.
                        if new_power_level != ApplicationActivityLevel::Inactive.into_primitive()
                            && boot_control_lease.borrow().is_some()
                        {
                            tracing::info!("System has booted. Dropping boot control lease.");
                            boot_control_lease.borrow_mut().take();
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

    fn run_full_wake_handling(self: &Rc<Self>, inspect_node: &fuchsia_inspect::Node) {
        let full_wake_handling_node = inspect_node.create_child("full_wake_handling");
        let this = self.clone();

        fasync::Task::local(async move {
            run_power_element(
                &this.full_wake_handling.name(),
                &this.full_wake_handling.required_level,
                FullWakeHandlingLevel::Inactive.into_primitive(),
                full_wake_handling_node,
                default_update_fn(&this.full_wake_handling),
            )
            .await;
        })
        .detach();
    }

    fn run_wake_handling(self: &Rc<Self>, inspect_node: &fuchsia_inspect::Node) {
        let wake_handling_node = inspect_node.create_child("wake_handling");
        let this = self.clone();

        fasync::Task::local(async move {
            run_power_element(
                this.wake_handling.name(),
                &this.wake_handling.required_level,
                WakeHandlingLevel::Inactive.into_primitive(),
                wake_handling_node,
                default_update_fn(&this.wake_handling),
            )
            .await;
        })
        .detach();
    }

    fn run_execution_resume_latency(self: &Rc<Self>, inspect_node: &fuchsia_inspect::Node) {
        let execution_resume_latency_node = inspect_node.create_child("execution_resume_latency");
        let initial_level = 0;
        let this = self.clone();

        let resume_latencies_node = execution_resume_latency_node
            .create_int_array("resume_latencies", self.resume_latencies.len());
        for (i, val) in self.resume_latencies.iter().enumerate() {
            resume_latencies_node.set(i, *val);
        }

        execution_resume_latency_node.record(resume_latencies_node);
        let resume_latency_node = Rc::new(
            execution_resume_latency_node.create_int("resume_latency", self.resume_latencies[0]),
        );

        fasync::Task::local(async move {
            let sag = this.clone();
            let update_fn = Rc::new(default_update_fn(&this.execution_resume_latency));

            run_power_element(
                this.execution_resume_latency.name(),
                &this.execution_resume_latency.required_level,
                initial_level,
                execution_resume_latency_node,
                Box::new(move |new_power_level: fbroker::PowerLevel| {
                    let sag = sag.clone();
                    let update_fn = update_fn.clone();
                    let resume_latency_node = resume_latency_node.clone();

                    async move {
                        // new_power_level for execution_resume_latency is an index into
                        // the list of resume latencies returned by the suspend HAL.

                        // Before other power elements are informed of the new power level,
                        // update the value that will be sent to the suspend HAL when suspension
                        // is triggered to avoid data races.
                        if (new_power_level as usize) < sag.resume_latencies.len() {
                            sag.execution_state_manager
                                .set_suspend_state_index(new_power_level.into())
                                .await;
                        }

                        update_fn(new_power_level).await;

                        // After other power elements are informed of the new power level,
                        // update Inspect to account for the new resume latency value.
                        let power_level = new_power_level as usize;
                        if power_level < sag.resume_latencies.len() {
                            resume_latency_node.set(sag.resume_latencies[power_level]);
                        }
                    }
                    .boxed_local()
                }),
            )
            .await;
        })
        .detach();
    }

    async fn run_fidl_server(self: &Rc<Self>) -> Result<()> {
        let mut service_fs = ServiceFs::new_local();

        service_fs
            .dir("svc")
            .add_fidl_service(IncomingRequest::ActivityGovernor)
            .add_fidl_service(IncomingRequest::Stats);
        service_fs
            .take_and_serve_directory_handle()
            .context("failed to serve outgoing namespace")?;

        service_fs
            .for_each_concurrent(None, move |request: IncomingRequest| {
                let sag = self.clone();
                async move {
                    match request {
                        IncomingRequest::ActivityGovernor(stream) => {
                            fasync::Task::local(sag.handle_activity_governor_request(stream))
                                .detach()
                        }
                        IncomingRequest::Stats(stream) => {
                            fasync::Task::local(sag.handle_stats_request(stream)).detach()
                        }
                    }
                }
            })
            .await;
        Ok(())
    }

    async fn handle_activity_governor_request(
        self: Rc<Self>,
        mut stream: fsystem::ActivityGovernorRequestStream,
    ) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsystem::ActivityGovernorRequest::GetPowerElements { responder } => {
                    let result = responder.send(fsystem::PowerElements {
                        execution_state: Some(fsystem::ExecutionState {
                            passive_dependency_token: Some(
                                self.execution_state_manager.passive_dependency_token(),
                            ),
                            ..Default::default()
                        }),
                        application_activity: Some(fsystem::ApplicationActivity {
                            active_dependency_token: Some(
                                self.application_activity.active_dependency_token(),
                            ),
                            ..Default::default()
                        }),
                        full_wake_handling: Some(fsystem::FullWakeHandling {
                            active_dependency_token: Some(
                                self.full_wake_handling.active_dependency_token(),
                            ),
                            ..Default::default()
                        }),
                        wake_handling: Some(fsystem::WakeHandling {
                            active_dependency_token: Some(
                                self.wake_handling.active_dependency_token(),
                            ),
                            ..Default::default()
                        }),
                        execution_resume_latency: Some(fsystem::ExecutionResumeLatency {
                            passive_dependency_token: Some(
                                self.execution_resume_latency.passive_dependency_token(),
                            ),
                            active_dependency_token: Some(
                                self.execution_resume_latency.active_dependency_token(),
                            ),
                            resume_latencies: Some(self.resume_latencies.clone()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    });

                    if let Err(error) = result {
                        tracing::warn!(
                            ?error,
                            "Encountered error while responding to GetPowerElements request"
                        );
                    }
                }
                fsystem::ActivityGovernorRequest::RegisterListener { responder, payload } => {
                    match payload.listener {
                        Some(listener) => {
                            self.listeners.borrow_mut().push(listener.into_proxy().unwrap());
                        }
                        None => tracing::warn!("No listener provided in request"),
                    }
                    let _ = responder.send();
                }
                fsystem::ActivityGovernorRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!(?ordinal, "Unknown ActivityGovernorRequest method");
                }
            }
        }
    }

    async fn handle_stats_request(self: Rc<Self>, mut stream: fsuspend::StatsRequestStream) {
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
}

#[async_trait(?Send)]
impl SuspendResumeListener for SystemActivityGovernor {
    fn suspend_stats(&self) -> &SuspendStatsManager {
        &self.suspend_stats
    }

    fn on_suspend(&self) {
        // A client may call RegisterListener while handling on_suspend which may cause another
        // mutable borrow of listeners. Clone the listeners to prevent this.
        let listeners: Vec<_> = self.listeners.borrow_mut().clone();
        for l in listeners {
            let _ = l.on_suspend();
        }
    }

    async fn on_resume(&self) {
        // A client may call RegisterListener while handling on_resume which may cause another
        // mutable borrow of listeners. Clone the listeners to prevent this.
        let listeners: Vec<_> = self.listeners.borrow_mut().clone();
        for l in listeners {
            let _ = l.on_resume().await;
        }
    }
}
