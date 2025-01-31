// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use fidl_fuchsia_power_system::CpuLevel;
use fuchsia_inspect::Node as INode;
use fuchsia_inspect_contrib::nodes::BoundedListNode as IRingBuffer;
use futures::channel::mpsc::{self, Receiver, Sender};
use futures::lock::Mutex;
use futures::{FutureExt, StreamExt};
use power_broker_client::{run_power_element, PowerElementContext};
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;
use {
    fidl_fuchsia_hardware_suspend as fhsuspend, fidl_fuchsia_power_broker as fbroker,
    fidl_fuchsia_power_observability as fobs, fidl_fuchsia_power_suspend as fsuspend,
    fidl_fuchsia_power_system as fsystem, fuchsia_async as fasync,
};

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

/// An updater for an [`fsuspend::SuspendStats`] object.
pub trait SuspendStatsUpdater {
    fn update<'a>(&self, update: Box<dyn FnOnce(&mut Option<fsuspend::SuspendStats>) -> bool + 'a>);
}

/// A listener for suspend/resume operations.
/// Also provides statistics about suspend/resume.
#[async_trait(?Send)]
pub trait SuspendResumeListener {
    /// Gets the manager of suspend stats.
    fn suspend_stats(&self) -> &dyn SuspendStatsUpdater;
    /// Leases (Execution State, Suspending). Called after system suspension ends.
    async fn on_suspend_ended(&self);
    /// Notify the listeners that system suspension is about to begin
    async fn notify_on_suspend(&self);
    /// Notify the listeners of a suspend success.
    async fn notify_on_resume(&self);
}

/// Vends "suspend blockers", object references that, when held, indicate that the system should
/// not suspend. Practically speaking, these blockers are used to guarantee that the system does
/// not suspend in the gap between when a wake lease is acquired and when the lease becomes
/// satisfied.
pub struct SuspendBlockManager {
    marker: Mutex<Rc<()>>,
}

impl SuspendBlockManager {
    pub fn new() -> Self {
        SuspendBlockManager { marker: Mutex::new(Rc::new(())) }
    }

    /// Returns a suspend blocker, possibly needing to wait until an in-flight suspend attempt
    /// completes.
    pub async fn get_blocker(&self) -> std::rc::Weak<()> {
        let marker = self.marker.lock().await;
        Rc::downgrade(&marker)
    }

    /// Attempts to acquire a suspend blocker immediately, returning None if the system is currently
    /// executing a suspend attempt.
    pub fn try_get_blocker(&self) -> Option<std::rc::Weak<()>> {
        self.marker.try_lock().map(|marker| Rc::downgrade(&marker))
    }

    /// If suspend is allowed, returns a guard that blocks further get_blocker() calls as long as it
    /// is held. Otherwise, returns None.
    pub(crate) fn suspend_allowed(&self) -> Option<futures::lock::MutexGuard<'_, Rc<()>>> {
        match self.marker.try_lock() {
            None => None,
            Some(marker) => {
                if Rc::weak_count(&marker) > 0 {
                    None
                } else {
                    Some(marker)
                }
            }
        }
    }
}

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
    cpu_element_is_inactive: bool,
    /// Allows the upper layer of SAG to block system suspension.
    suspend_block_manager: Rc<SuspendBlockManager>,
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
                cpu_element_is_inactive: false,
                suspend_block_manager: Rc::new(SuspendBlockManager::new()),
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

    /// Updates the power level of the CPU power element.
    ///
    /// Returns a Result that indicates whether the system should suspend or not.
    /// If an error occurs while updating the power level, the error is forwarded to the caller.
    pub async fn update_current_level(&self, required_level: fbroker::PowerLevel) -> Result<bool> {
        log::debug!(required_level:?; "update_current_level: acquiring inner lock");
        let mut inner = self.inner.lock().await;

        log::debug!(required_level:?; "update_current_level: updating current level");
        let res = inner.cpu.current_level.update(required_level).await;
        if let Err(error) = res {
            log::warn!(error:?; "update_current_level: current_level.update failed");
            return Err(error.into());
        }

        // After other elements have been informed of required_level for cpu,
        // check whether the system can be suspended.
        if required_level == CpuLevel::Inactive.into_primitive() {
            log::debug!("beginning suspend process for cpu");
            inner.cpu_element_is_inactive = true;
            return Ok(true);
        } else {
            inner.cpu_element_is_inactive = false;
            return Ok(false);
        }
    }

    /// Gets a copy of the name of the CPU power element.
    async fn name(&self) -> String {
        self.inner.lock().await.cpu.name().to_string()
    }

    /// Gets a copy of the RequiredLevelProxy of the CPU power element.
    async fn required_level_proxy(&self) -> fbroker::RequiredLevelProxy {
        self.inner.lock().await.cpu.required_level.clone()
    }

    pub async fn cpu(&self) -> Rc<PowerElementContext> {
        self.inner.lock().await.cpu.clone()
    }

    pub async fn suspend_block_manager(&self) -> Rc<SuspendBlockManager> {
        self.inner.lock().await.suspend_block_manager.clone()
    }

    /// Attempts to suspend the system.
    ///
    /// Returns an enum representing the result of the suspend attempt.
    pub async fn trigger_suspend(&self) -> SuspendResult {
        let listener = self.suspend_resume_listener.get().unwrap();
        let mut suspend_failed = false;
        {
            log::debug!("trigger_suspend: acquiring inner lock");
            let inner = self.inner.lock().await;
            if !inner.cpu_element_is_inactive {
                log::info!("Suspend not allowed because CPU element is not inactive");
                return SuspendResult::NotAllowed;
            }

            let _lock = match inner.suspend_block_manager.suspend_allowed() {
                Some(lock) => lock,
                None => {
                    log::info!("Suspend not allowed due to outstanding wake leases");
                    return SuspendResult::NotAllowed;
                }
            };

            self._inspect_node.borrow_mut().add_entry(|node| {
                node.record_int(
                    fobs::SUSPEND_ATTEMPTED_AT,
                    zx::MonotonicInstant::get().into_nanos(),
                );
            });
            // LINT.IfChange
            log::info!("Suspending");
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
            log::info!(response:?; "Resuming");
            // LINT.ThenChange(//src/testing/end_to_end/honeydew/honeydew/affordances/starnix/system_power_state_controller.py)
            self._inspect_node.borrow_mut().add_entry(|node| {
                let time = zx::MonotonicInstant::get().into_nanos();
                if let Some(Ok(Ok(fhsuspend::SuspenderSuspendResponse {
                    suspend_duration: Some(duration),
                    ..
                }))) = response
                {
                    node.record_int(fobs::SUSPEND_RESUMED_AT, time);
                    node.record_int(fobs::SUSPEND_LAST_TIMESTAMP, duration);
                } else {
                    node.record_int(fobs::SUSPEND_FAILED_AT, time);
                }
            });

            listener.suspend_stats().update(Box::new(
                |stats_opt: &mut Option<fsuspend::SuspendStats>| {
                    let stats = stats_opt.as_mut().expect("stats is uninitialized");

                    match response {
                        Some(Ok(Ok(res))) => {
                            stats.last_time_in_suspend = res.suspend_duration;
                            stats.last_time_in_suspend_operations = res.suspend_overhead;

                            if stats.last_time_in_suspend.is_some() {
                                stats.success_count = stats.success_count.map(|c| c + 1);
                            } else {
                                log::warn!("Failed to suspend in Suspender");
                                suspend_failed = true;
                                stats.fail_count = stats.fail_count.map(|c| c + 1);
                            }
                        }
                        Some(error) => {
                            log::warn!(error:?; "Failed to suspend");
                            stats.fail_count = stats.fail_count.map(|c| c + 1);
                            suspend_failed = true;

                            if let Ok(Err(error)) = error {
                                stats.last_failed_error = Some(error);
                            }
                        }
                        None => {
                            log::warn!("No suspender available, suspend was a no-op");
                            stats.fail_count = stats.fail_count.map(|c| c + 1);
                            stats.last_failed_error = Some(zx::sys::ZX_ERR_NOT_SUPPORTED);
                        }
                    }
                    true
                },
            ));
        }
        // At this point, the suspend request is no longer in flight and has been handled. With
        // `inner` going out of scope, other tasks can modify flags and update the power level of
        // CPU power element.
        listener.on_suspend_ended().await;
        if suspend_failed {
            SuspendResult::Fail
        } else {
            SuspendResult::Success
        }
    }

    pub fn run(self: &Rc<Self>, power_elements_node: &INode) {
        let (suspend_tx, suspend_rx) = mpsc::channel(1);
        self.run_suspend_task(suspend_rx);
        self.run_power_element(power_elements_node, suspend_tx);
    }

    fn run_suspend_task(self: &Rc<Self>, mut suspend_signal: Receiver<()>) {
        let cpu_manager = self.clone();

        fasync::Task::local(async move {
            loop {
                log::debug!("awaiting suspend signals");
                suspend_signal.next().await;
                log::debug!("attempting to suspend");
                log::info!("trigger_suspend result: {:?}", cpu_manager.trigger_suspend().await);
            }
        })
        .detach();
    }

    pub fn run_power_element(
        self: &Rc<Self>,
        power_elements_node: &INode,
        suspend_signaller: Sender<()>,
    ) {
        let cpu_manager = self.clone();
        let cpu_node = power_elements_node.create_child("cpu");

        fasync::Task::local(async move {
            let element_name = cpu_manager.name().await;
            let required_level = cpu_manager.required_level_proxy().await;

            run_power_element(
                &element_name,
                &required_level,
                fsystem::CpuLevel::Inactive.into_primitive(),
                Some(cpu_node),
                Box::new(move |new_power_level: fbroker::PowerLevel| {
                    let cpu_manager = cpu_manager.clone();
                    let mut suspend_signaller = suspend_signaller.clone();

                    async move {
                        let update_res = cpu_manager.update_current_level(new_power_level).await;
                        if let Ok(true) = update_res {
                            let _ = suspend_signaller.start_send(());
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
